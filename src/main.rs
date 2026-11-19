mod cli;
mod config;
mod exec;
mod git;
mod manage;
mod migrate;
mod render;
mod scan;
mod status;
mod target;

use anyhow::{Result, bail};
use clap::{ArgMatches, Command};
use config::{CmdDef, Settings, State};
use indexmap::IndexMap;
use std::{ffi::OsString, fmt, num::NonZeroUsize, path::Path, process::ExitCode};

#[derive(Debug)]
pub struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CliError {}

fn delegates(settings: &Settings) -> Result<IndexMap<String, CmdDef>> {
    let mut commands: IndexMap<String, CmdDef> =
        serde_json::from_str(include_str!("../assets/default_commands.json"))?;
    for (name, def) in &settings.commands {
        if cli::BUILTINS.contains(&name.as_str()) {
            eprintln!("Warning: ignoring command definition for built-in command {name}");
        } else if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_whitespace)
        {
            bail!("settings.json: invalid command name {name:?}");
        } else {
            commands.insert(name.clone(), def.clone());
        }
    }
    Ok(commands)
}

fn mutates(name: &str, args: &ArgMatches) -> bool {
    match name {
        "rm" | "rename" | "clear" => true,
        "add" | "scan" | "clone" => !args.get_flag("dry"),
        "context" => args.contains_id("group"),
        "group" | "flags" | "color" | "info" => args
            .subcommand_name()
            .is_some_and(|s| !matches!(s, "ls" | "ll")),
        _ => false,
    }
}

fn raw_after_command(argv: &[OsString]) -> &[OsString] {
    let mut index = 1;
    while let Some(arg) = argv.get(index).and_then(|arg| arg.to_str()) {
        match arg {
            "-j" | "--jobs" => index += 2,
            option if option.starts_with("--jobs=") || option.starts_with("-j") => index += 1,
            _ => return &argv[index + 1..],
        }
    }
    &[]
}

fn run_targets(
    state: &State,
    cwd: &Path,
    jobs: Option<usize>,
    name: &str,
    args: &ArgMatches,
    argv: &[OsString],
    commands: &IndexMap<String, CmdDef>,
) -> Result<()> {
    let context = target::context(state, cwd);
    if let Some(def) = commands.get(name) {
        let names = manage::words(args, "repos");
        if names.is_empty() && context.is_none() && !def.allow_all {
            return Err(CliError("Specify at least one repo or group".into()).into());
        }
        let repos = target::repos(state, &names, context);
        return exec::run(
            &repos,
            &exec::Invocation::delegated(def, args.get_flag("shell"))?,
            def.disable_async,
            false,
            jobs,
        );
    }
    let words: Vec<_> = raw_after_command(argv)
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let (names, remaining) = target::split(state, &words)?;
    let repos = target::repos(state, &names, context);
    if name == "shell" {
        exec::run(
            &repos,
            &exec::Invocation::Shell(remaining.join(" ")),
            false,
            true,
            jobs,
        )
    } else {
        let sequential = matches!(remaining[0], "log" | "show" | "difftool" | "mergetool");
        let invocation = exec::Invocation::Argv(
            std::iter::once("git")
                .chain(remaining)
                .map(str::to_owned)
                .collect(),
        );
        exec::run(&repos, &invocation, sequential, false, jobs)
    }
}

fn app_args(app: &Command, argv: &[OsString]) -> Result<ArgMatches> {
    match app.clone().try_get_matches_from(argv) {
        Ok(args) => Ok(args),
        Err(error) => error.exit(),
    }
}

fn run() -> Result<u8> {
    let argv: Vec<_> = std::env::args_os().collect();
    if argv.len() == 2 && matches!(argv[1].to_str(), Some("-v" | "--version")) {
        println!("gita {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    let dir = config::directory()?;
    let mut state = State::read(&dir)?;
    let mut settings = Settings::read(&dir)?;
    let mut commands = delegates(&settings)?;
    let mut app = cli::build(&state, &commands);
    clap_complete::CompleteEnv::with_factory(|| app.clone()).complete();
    let mut args = app_args(&app, &argv)?;
    let Some((name, sub)) = args.subcommand() else {
        if args.get_flag("version") {
            println!("gita {}", env!("CARGO_PKG_VERSION"));
        } else {
            app.print_help()?;
            println!();
        }
        return Ok(0);
    };
    let _lock = if mutates(name, sub) {
        let lock = config::lock(&dir)?;
        state = State::read(&dir)?;
        settings = Settings::read(&dir)?;
        commands = delegates(&settings)?;
        app = cli::build(&state, &commands);
        args = app_args(&app, &argv)?;
        Some(lock)
    } else {
        None
    };
    let (name, sub) = args.subcommand().unwrap();
    let jobs = settings.jobs(args.get_one::<NonZeroUsize>("jobs").map(|n| n.get()))?;
    let cwd = std::env::current_dir()?;
    let result = match name {
        "add" | "scan" => manage::register(&mut state, sub, name == "scan", jobs),
        "clone" => migrate::clone_repos(&mut state, sub, &cwd, jobs),
        "ll" => render::list(
            &state,
            &settings,
            manage::word(sub, "group"),
            &cwd,
            sub.get_flag("grouped"),
            !sub.get_flag("no-colors"),
            jobs,
        )
        .map(|_| (false, false)),
        "freeze" => migrate::freeze(&state, &settings, manage::word(sub, "group"), &cwd, jobs)
            .map(|_| (false, false)),
        "color" | "info" => {
            manage::settings_command(&dir, &settings, name, sub).map(|_| (false, false))
        }
        "completion" => {
            completion(manage::word(sub, "shell").unwrap())?;
            Ok((false, false))
        }
        _ if name == "super" || name == "shell" || commands.contains_key(name) => {
            run_targets(&state, &cwd, jobs, name, sub, &argv, &commands).map(|_| (false, false))
        }
        _ => manage::state_command(&mut state, name, sub, &cwd).map(|changed| (changed, false)),
    };
    let (changed, failed) = match result {
        Err(error) if error.is::<CliError>() => {
            let command = app.find_subcommand_mut(name).unwrap();
            eprintln!("error: {error}\n\n{}", command.render_usage());
            return Ok(2);
        }
        other => other?,
    };
    if changed {
        state.save(&dir)?;
    }
    Ok(u8::from(failed))
}

fn completion(shell: &str) -> Result<()> {
    use clap_complete::env::{Bash, EnvCompleter, Fish, Powershell, Zsh};
    let generator: &dyn EnvCompleter = match shell {
        "bash" => &Bash,
        "zsh" => &Zsh,
        "fish" => &Fish,
        "powershell" => &Powershell,
        _ => unreachable!(),
    };
    generator.write_registration("COMPLETE", "gita", "gita", "gita", &mut std::io::stdout())?;
    Ok(())
}

fn main() -> ExitCode {
    #[cfg(windows)]
    let _ = enable_ansi_support::enable_ansi_support();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
