use crate::{
    config::{Group, Repo, Settings, State},
    exec, git, manage,
    status::{self, Query},
    target,
};
use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
struct Archive {
    version: u32,
    repos: Vec<Entry>,
    #[serde(default)]
    groups: IndexMap<String, Group>,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    name: String,
    url: String,
    #[serde(flatten)]
    repo: Repo,
    #[serde(default)]
    branch: String,
}

pub fn freeze(
    state: &State,
    settings: &Settings,
    group: Option<&str>,
    cwd: &Path,
    jobs: Option<usize>,
) -> Result<()> {
    let targets: Vec<_> = target::repos(state, &[], group.or_else(|| target::context(state, cwd)))
        .into_iter()
        .filter(|(_, repo)| repo.path.is_dir())
        .collect();
    let mut statuses = Vec::new();
    status::collect(
        &targets,
        settings.backend()?,
        Query::Freeze,
        jobs,
        |index, status| statuses.push((index, status)),
    );
    statuses.sort_by_key(|(i, _)| *i);
    let mut seen = HashSet::new();
    let mut repos = Vec::new();
    let mut failed = false;
    for (index, status) in statuses {
        let (name, repo) = targets[index];
        match status {
            Ok(status) => {
                if let Some(url) = status.url.filter(|url| seen.insert(url.clone())) {
                    repos.push(Entry {
                        name: name.into(),
                        url,
                        repo: repo.clone(),
                        branch: status.branch,
                    });
                }
            }
            Err(error) => {
                failed = true;
                eprintln!("{name}: {error:#}");
            }
        }
    }
    let names: HashSet<_> = repos.iter().map(|repo| repo.name.as_str()).collect();
    let groups = state
        .groups
        .iter()
        .filter_map(|(name, group)| {
            let mut group = group.clone();
            group.repos.retain(|repo| names.contains(repo.as_str()));
            (!group.repos.is_empty()).then(|| (name.clone(), group))
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&Archive {
            version: 1,
            repos,
            groups
        })?
    );
    if failed {
        bail!("Failed to read one or more repositories");
    }
    Ok(())
}

fn clone_name(url: &str) -> Result<&str> {
    let trimmed = url.trim_end_matches(['/', '\\']);
    let leaf = trimmed.rsplit(['/', '\\', ':']).next().unwrap();
    let name = leaf.strip_suffix(".git").unwrap_or(leaf);
    if name.is_empty()
        || matches!(name, "." | "..")
        || !Path::new(name).file_name().is_some_and(|leaf| leaf == name)
    {
        bail!("Cannot determine a destination name for {url}");
    }
    Ok(name)
}

pub fn clone_repos(
    state: &mut State,
    args: &ArgMatches,
    cwd: &Path,
    jobs: Option<usize>,
) -> Result<(bool, bool)> {
    let source = manage::word(args, "source").unwrap();
    let dir = args
        .get_one::<PathBuf>("directory")
        .map(|path| manage::abs(path))
        .transpose()?
        .unwrap_or_else(|| cwd.to_owned());
    let file = args.get_flag("file");
    let preserve = args.get_flag("paths");
    let dry = args.get_flag("dry");
    let group = manage::word(args, "group");
    if let Some(group) = group {
        manage::check_group(state, group, true)?;
    }
    let archive = if file {
        let data = fs::read(source).with_context(|| format!("Cannot read {source}"))?;
        let header: serde_json::Value =
            serde_json::from_slice(&data).with_context(|| format!("Invalid archive {source}"))?;
        if header["version"]
            .as_u64()
            .is_some_and(|version| version > 1)
        {
            bail!("{source} was created with a newer gita");
        }
        serde_json::from_slice(&data).with_context(|| format!("Invalid archive {source}"))?
    } else {
        let name = clone_name(source)?;
        Archive {
            version: 1,
            repos: vec![Entry {
                name: name.into(),
                url: source.to_owned(),
                repo: Repo::default(),
                branch: String::new(),
            }],
            groups: IndexMap::new(),
        }
    };
    let mut by_path: IndexMap<_, _> = state
        .repos
        .iter()
        .map(|(name, repo)| (target::path_key(&repo.path), name.clone()))
        .collect();
    if !file
        && !dry
        && state
            .repos
            .values()
            .filter(|repo| repo.path.is_dir())
            .any(|repo| {
                git::query(repo, &["remote", "get-url", "origin"])
                    .is_ok_and(|url| url.as_deref() == Some(source))
            })
    {
        println!("{source} already in gita.");
        return Ok((false, false));
    }
    let mut destinations = IndexSet::new();
    let mut tasks = Vec::new();
    let mut names = IndexMap::new();
    let mut failed = false;
    for entry in &archive.repos {
        let destination = if preserve {
            manage::abs(&entry.repo.path)?
        } else {
            dir.join(clone_name(&entry.url)?)
        };
        let key = target::path_key(&destination);
        if let Some(group) = group
            && !by_path.contains_key(&key)
            && manage::available_name(state, &destination, false) == group
        {
            bail!("{group} is already in use as the new repository name!");
        }
        if !dry {
            if let Some(name) = by_path.get(&key) {
                names.insert(entry.name.clone(), name.clone());
                if !file {
                    println!("{source} already in gita.");
                }
                continue;
            }
            if !destinations.insert(key) {
                eprintln!(
                    "{}: multiple repositories use the same destination",
                    destination.display()
                );
                failed = true;
                continue;
            }
        }
        if dry {
            print!("git clone {}", exec::shell_escape(&entry.url));
            if preserve {
                print!(" {}", exec::shell_escape(&destination.to_string_lossy()));
            }
            println!();
        } else {
            tasks.push((entry, destination));
        }
    }
    if dry {
        return Ok((false, failed));
    }
    let mut results = Vec::new();
    exec::parallel(
        &tasks,
        jobs.unwrap_or(tasks.len()),
        |(entry, destination)| clone_one(entry, destination, &dir, file),
        |index, result| results.push((index, result)),
    );
    results.sort_by_key(|(index, _)| *index);
    let mut changed = false;
    for (index, (created, result)) in results {
        let (entry, destination) = &tasks[index];
        if let Err(error) = result {
            eprintln!("{}: {error:#}", entry.name);
            failed = true;
        }
        if !created {
            continue;
        }
        let repo = Repo {
            path: destination.clone(),
            kind: entry.repo.kind.clone(),
            flags: entry.repo.flags.clone(),
        };
        let name = if file && !target::named(state, &entry.name) && !entry.name.is_empty() {
            entry.name.clone()
        } else {
            manage::available_name(state, destination, false)
        };
        git::warn_collision(&name);
        state.repos.insert(name.clone(), repo);
        by_path.insert(target::path_key(destination), name.clone());
        names.insert(entry.name.clone(), name);
        changed = true;
    }
    if let Some(group) = group
        && !names.is_empty()
    {
        manage::add_group(state, group, names.values().cloned(), None);
        changed = true;
    }
    for (name, group) in archive.groups {
        let members: Vec<_> = group
            .repos
            .iter()
            .filter_map(|member| names.get(member).cloned())
            .collect();
        if members.is_empty() {
            continue;
        }
        if let Err(error) = manage::check_group(state, &name, true) {
            eprintln!("{error:#}");
            failed = true;
            continue;
        }
        git::warn_collision(&name);
        manage::add_group(state, &name, members, group.path);
        changed = true;
    }
    Ok((changed, failed))
}

fn clone_one(
    entry: &Entry,
    destination: &Path,
    dir: &Path,
    background: bool,
) -> (bool, Result<()>) {
    let clone = || -> Result<()> {
        let parent = destination
            .parent()
            .context("Missing clone destination parent")?;
        fs::create_dir_all(parent)?;
        let mut command = std::process::Command::new(git::executable()?);
        command.current_dir(if dir.is_dir() { dir } else { parent });
        command.args(["clone", "--", &entry.url]).arg(destination);
        if background {
            exec::background(&mut command);
        }
        if !command.status()?.success() {
            bail!("git clone failed");
        }
        Ok(())
    };
    if let Err(error) = clone() {
        return (false, Err(error));
    }
    let checkout = (|| -> Result<()> {
        if entry.branch.is_empty() {
            return Ok(());
        }
        let repo = Repo {
            path: destination.to_owned(),
            ..Repo::default()
        };
        let mut command = git::command(&repo)?;
        // Resolve to a local branch, remote-tracking branch, or tag before passing it to checkout.
        // '--' at the end keeps checkout from interpreting a failed revision as a pathspec.
        if entry.branch.starts_with('-') {
            bail!("Invalid checkout ref {}", entry.branch);
        }
        command.args(["checkout", &entry.branch, "--"]);
        if background {
            exec::background(&mut command);
        }
        if !command.status()?.success() {
            bail!("git checkout {} failed", entry.branch);
        }
        Ok(())
    })();
    (true, checkout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clone_destination_names_follow_git_conventions() {
        assert_eq!(
            clone_name("git@example.org:org/project.git").unwrap(),
            "project"
        );
        assert_eq!(
            clone_name("https://example.org/org/project.git/").unwrap(),
            "project"
        );
        assert_eq!(clone_name(r"C:\repos\project.git").unwrap(), "project");
        assert!(clone_name("..").is_err());
    }
}
