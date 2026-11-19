use crate::config::Repo;
use anyhow::{Result, anyhow};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

pub fn executable() -> Result<&'static Path> {
    static GIT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    GIT.get_or_init(|| {
        which::which("git").map_err(|e| format!("Git 2.35 or later is required on PATH: {e}"))
    })
    .as_deref()
    .map_err(|e| anyhow!("{e}"))
}

pub fn command(repo: &Repo) -> Result<Command> {
    let mut command = Command::new(executable()?);
    command.current_dir(&repo.path).args(&repo.flags);
    Ok(command)
}

pub fn output(repo: &Repo, args: &[&str]) -> Result<Output> {
    Ok(command(repo)?.args(args).output()?)
}

pub fn string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

pub fn query(repo: &Repo, args: &[&str]) -> Result<Option<String>> {
    let result = output(repo, args)?;
    Ok(result.status.success().then(|| string(&result)))
}

pub fn is_repo(path: &Path, bare: bool, skip_submodule: bool) -> bool {
    if !path.is_dir() {
        return false;
    }
    let dotgit = path.join(".git");
    if dotgit.exists() {
        return gix::discover::is_git(&dotgit).is_ok()
            && !(skip_submodule
                && dotgit.is_file()
                && std::fs::read_to_string(dotgit)
                    .is_ok_and(|s| s.replace('\\', "/").contains(".git/modules/")));
    }
    bare && path.join("HEAD").is_file()
        && query(
            &Repo {
                path: path.to_owned(),
                ..Repo::default()
            },
            &["rev-parse", "--is-bare-repository"],
        )
        .is_ok_and(|value| value.as_deref() == Some("true"))
}

pub fn is_command(name: &str) -> bool {
    static COMMANDS: OnceLock<Vec<String>> = OnceLock::new();
    COMMANDS
        .get_or_init(|| {
            executable()
                .ok()
                .and_then(|git| {
                    Command::new(git)
                        .arg("--list-cmds=main,others,alias")
                        .output()
                        .ok()
                })
                .map(|out| string(&out).split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default()
        })
        .iter()
        .any(|command| command == name)
}

pub fn warn_collision(name: &str) {
    if is_command(name) {
        eprintln!("Warning: {name} is also a Git command or alias. Use -- before Git commands.");
    }
}
