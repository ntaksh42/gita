use crate::{
    config::{CmdDef, CmdSpec, Repo},
    git,
};
use anyhow::{Context, Result, bail};
use std::{
    io::{self, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

pub fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

pub fn parallel<T: Sync, R: Send>(
    tasks: &[T],
    jobs: usize,
    work: impl Fn(&T) -> R + Sync,
    mut done: impl FnMut(usize, R),
) {
    if tasks.is_empty() {
        return;
    }
    if jobs == 1 || tasks.len() == 1 {
        for (index, task) in tasks.iter().enumerate() {
            done(index, work(task));
        }
        return;
    }
    let next = AtomicUsize::new(0);
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(tasks.len()) {
            let (send, next, work) = (send.clone(), &next, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(index) else { break };
                    if send.send((index, work(task))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(send);
        for (index, result) in receive {
            done(index, result);
        }
    });
}

pub enum Invocation {
    Argv(Vec<String>),
    Shell(String),
}

impl Invocation {
    pub fn delegated(def: &CmdDef, force_shell: bool) -> Result<Self> {
        let shell = def.shell || force_shell;
        match (&def.cmd, shell) {
            (CmdSpec::Line(line), true) => Ok(Self::Shell(line.clone())),
            (CmdSpec::Argv(args), true) => Ok(Self::Shell(
                args.iter()
                    .map(|arg| shell_escape(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
            (CmdSpec::Line(line), false) => Ok(Self::Argv(
                line.split_whitespace().map(str::to_owned).collect(),
            )),
            (CmdSpec::Argv(args), false) => Ok(Self::Argv(args.clone())),
        }
    }

    fn command(&self, repo: &Repo) -> Result<Command> {
        match self {
            Self::Argv(args) => {
                let program = args
                    .first()
                    .filter(|arg| !arg.is_empty())
                    .context("Empty command definition")?;
                let mut command = if program == "git" {
                    git::command(repo)?
                } else {
                    let mut command = Command::new(program);
                    command.current_dir(&repo.path);
                    command
                };
                command.args(&args[1..]);
                Ok(command)
            }
            Self::Shell(line) => Ok(shell_command(repo, line)),
        }
    }
}

#[cfg(windows)]
fn shell_command(repo: &Repo, line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd.exe");
    command
        .current_dir(&repo.path)
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{line}\""));
    command
}

#[cfg(not(windows))]
fn shell_command(repo: &Repo, line: &str) -> Command {
    let mut command = Command::new("sh");
    command.current_dir(&repo.path).args(["-c", line]);
    command
}

pub fn shell_escape(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/:._-@+".contains(c))
    {
        return arg.to_owned();
    }
    if cfg!(windows) {
        format!("\"{}\"", arg.replace('"', "\"\""))
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

pub fn background(command: &mut Command) {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setsid is async-signal-safe between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn foreground(repo: &Repo, invocation: &Invocation) -> Result<bool> {
    println!("{}", repo.path.display());
    io::stdout().flush()?;
    Ok(invocation.command(repo)?.status()?.success())
}

pub fn run(
    repos: &[(&str, &Repo)],
    invocation: &Invocation,
    sequential: bool,
    force_capture: bool,
    jobs: Option<usize>,
) -> Result<()> {
    let repos: Vec<_> = repos
        .iter()
        .copied()
        .filter(|(_, repo)| repo.path.is_dir())
        .collect();
    let mut failed = false;
    let mut retries = Vec::new();
    if sequential || (!force_capture && (repos.len() == 1 || jobs == Some(1))) {
        retries.extend(0..repos.len());
    } else {
        let mut write_error = None;
        parallel(
            &repos,
            jobs.unwrap_or(repos.len()),
            |(_, repo)| {
                let mut command = invocation.command(repo)?;
                background(&mut command);
                Ok::<_, anyhow::Error>(command.output()?)
            },
            |index, result| {
                let name = repos[index].0;
                match result {
                    Ok(output) => {
                        let mut block = Vec::new();
                        for stream in [&output.stdout, &output.stderr] {
                            for line in stream.split_inclusive(|&c| c == b'\n') {
                                block.extend_from_slice(name.as_bytes());
                                block.extend_from_slice(b": ");
                                block.extend_from_slice(line);
                                if !line.ends_with(b"\n") {
                                    block.push(b'\n');
                                }
                            }
                        }
                        let result = io::stdout().lock().write_all(&block);
                        if let Err(error) = result {
                            write_error = Some(error);
                        }
                        if !output.status.success() {
                            retries.push(index);
                        }
                    }
                    Err(error) => {
                        eprintln!("{name}: {error:#}");
                        retries.push(index);
                    }
                }
            },
        );
        if let Some(error) = write_error {
            return Err(error.into());
        }
        retries.sort_unstable();
    }
    for index in retries {
        let (name, repo) = repos[index];
        match foreground(repo, invocation) {
            Ok(success) => failed |= !success,
            Err(error) => {
                eprintln!("{name}: {error:#}");
                failed = true;
            }
        }
    }
    if failed {
        bail!("Command failed in one or more repositories");
    }
    Ok(())
}
