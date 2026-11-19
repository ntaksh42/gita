use super::{Query, Situation, Status};
use crate::{config::Repo, git};
use anyhow::{Result, anyhow, bail};

pub fn read(repo: &Repo, query: Query, concurrent: bool) -> Result<Status> {
    match query {
        Query::Freeze => {
            let remote = git::query(repo, &["remote", "get-url", "origin"])?;
            let url = if remote.is_some() {
                remote
            } else {
                let names = git::query(repo, &["remote"])?;
                match names.as_deref().and_then(|names| names.lines().next()) {
                    Some(name) => git::query(repo, &["remote", "get-url", name])?,
                    None => None,
                }
            };
            Ok(Status {
                branch: branch(repo)?,
                url,
                ..Status::default()
            })
        }
        Query::Status { commit } => {
            let log = || git::query(repo, &["log", "-1", "--format=%s%x1f%cr"]);
            let (mut status, details) = if commit && concurrent {
                std::thread::scope(|scope| {
                    let log = scope.spawn(log);
                    let status = read_status(repo);
                    Ok::<_, anyhow::Error>((status?, log.join().unwrap()?))
                })?
            } else {
                (read_status(repo)?, if commit { log()? } else { None })
            };
            if let Some((message, time)) = details
                .as_deref()
                .and_then(|details| details.rsplit_once('\x1f'))
            {
                status.message = message.to_owned();
                status.time = time.to_owned();
            }
            Ok(status)
        }
    }
}

fn tag(repo: &Repo) -> Result<String> {
    Ok(git::query(repo, &["describe", "--tags", "--exact-match"])?.unwrap_or_default())
}

fn branch(repo: &Repo) -> Result<String> {
    match git::query(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])? {
        Some(name) => Ok(name),
        None => tag(repo),
    }
}

fn read_status(repo: &Repo) -> Result<Status> {
    let out = git::output(
        repo,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--show-stash",
            "-z",
            "--untracked-files=normal",
        ],
    )?;
    if out.status.success() {
        let mut status = parse(&out.stdout);
        if status.branch == "(detached)" {
            status.branch = tag(repo)?;
        }
        return Ok(status);
    }
    if git::query(repo, &["rev-parse", "--is-bare-repository"])?.as_deref() == Some("true") {
        let counts = git::query(
            repo,
            &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        )?;
        let situation = counts
            .as_deref()
            .and_then(|counts| {
                let mut counts = counts.split_whitespace();
                Some(Situation::counts(
                    counts.next()?.parse().ok()?,
                    counts.next()?.parse().ok()?,
                ))
            })
            .unwrap_or_default();
        return Ok(Status {
            branch: branch(repo)?,
            situation,
            bare: true,
            ..Status::default()
        });
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(version) = git::query(repo, &["--version"])?
        && git_too_old(&version)
    {
        bail!("Git 2.35 or later is required (found {version})");
    }
    Err(anyhow!("{}", stderr.trim()))
}

fn git_too_old(version: &str) -> bool {
    let Some(version) = version.strip_prefix("git version ") else {
        return false;
    };
    let mut digits = version.split('.').filter_map(|n| n.parse::<u32>().ok());
    digits
        .next()
        .zip(digits.next())
        .is_some_and(|v| v < (2, 35))
}

pub fn parse(data: &[u8]) -> Status {
    let mut s = Status::default();
    let mut ab = None;
    let mut upstream = false;
    let mut records = data.split(|&b| b == 0);
    while let Some(record) = records.next() {
        if let Some(value) = record.strip_prefix(b"# branch.head ") {
            s.branch = String::from_utf8_lossy(value).into_owned();
        } else if record.starts_with(b"# branch.upstream ") {
            upstream = true;
        } else if let Some(value) = record.strip_prefix(b"# branch.ab ") {
            let value = String::from_utf8_lossy(value);
            ab = value.split_once(" -").and_then(|(a, b)| {
                Some(Situation::counts(
                    a.strip_prefix('+')?.parse().ok()?,
                    b.parse().ok()?,
                ))
            });
        } else if let Some(value) = record.strip_prefix(b"# stash ") {
            s.stashed = value != b"0";
        } else {
            match record {
                [kind @ (b'1' | b'2'), b' ', x, y, ..] => {
                    s.staged |= *x != b'.';
                    s.dirty |= *y != b'.';
                    if *kind == b'2' {
                        records.next();
                    }
                }
                [b'u', b' ', ..] => {
                    s.dirty = true;
                    s.staged = true;
                }
                [b'?', b' ', ..] => s.untracked = true,
                _ => {}
            }
        }
    }
    if upstream {
        s.situation = ab.unwrap_or_default();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_v2_distinguishes_staged_dirty_stash_and_rename_source() {
        let s = parse(b"# branch.head develop\0# branch.upstream origin/develop\0# branch.ab +3 -1\0# stash 2\0\
            1 .M metadata file\0\
            2 R. metadata new name\0? old name\0");
        assert_eq!(s.branch, "develop");
        assert_eq!(s.situation, Situation::Diverged);
        assert!(s.dirty && s.staged && s.stashed && !s.untracked);
        let s = parse(b"u UU unmerged\0? untracked\nfile\0! ignored\0");
        assert!(s.dirty && s.staged && s.untracked);
        assert!(!s.stashed);
    }

    #[test]
    fn porcelain_v2_requires_upstream_and_counts() {
        for (counts, situation) in [
            ("+0 -0", Situation::InSync),
            ("+1 -0", Situation::LocalAhead),
            ("+0 -1", Situation::RemoteAhead),
            ("+1 -1", Situation::Diverged),
            ("+? -?", Situation::NoRemote),
        ] {
            assert_eq!(
                parse(format!("# branch.upstream origin/main\0# branch.ab {counts}\0").as_bytes())
                    .situation,
                situation
            );
        }
        assert_eq!(parse(b"# branch.ab +0 -0\0").situation, Situation::NoRemote);
        assert_eq!(
            parse(b"# branch.upstream origin/main\0").situation,
            Situation::NoRemote
        );
        assert!(git_too_old("git version 2.34.9.windows.1"));
        assert!(!git_too_old("git version 2.35.0.windows.1"));
    }
}
