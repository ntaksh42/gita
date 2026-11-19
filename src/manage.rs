use crate::{
    config::{self, Group, INFO, Repo, Settings, State},
    git, render, scan, target,
};
use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use indexmap::IndexSet;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub fn word<'a>(args: &'a ArgMatches, id: &str) -> Option<&'a str> {
    args.get_one::<String>(id).map(String::as_str)
}
pub fn words<'a>(args: &'a ArgMatches, id: &str) -> Vec<&'a str> {
    args.get_many::<String>(id)
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect()
}
pub fn abs(path: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}
fn leaf(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn check_group(state: &State, name: &str, allow_existing: bool) -> Result<()> {
    if name.is_empty()
        || matches!(name, "none" | "auto")
        || state.repos.contains_key(name)
        || (!allow_existing && state.groups.contains_key(name))
    {
        bail!("{name} is already in use or reserved!");
    }
    Ok(())
}

pub fn add_group(
    state: &mut State,
    name: &str,
    repos: impl IntoIterator<Item = String>,
    path: Option<PathBuf>,
) {
    let group = state.groups.entry(name.to_owned()).or_default();
    if path.is_some() {
        group.path = path;
    }
    group.repos.extend(repos);
    group.repos.sort();
    group.repos.dedup();
}

pub fn available_name(state: &State, path: &Path, ambiguous: bool) -> String {
    let free = |name: &str| !target::named(state, name);
    let base = leaf(path);
    if !ambiguous && free(&base) {
        return base;
    }
    let mut name = base;
    for ancestor in path.ancestors().skip(1).filter_map(Path::file_name) {
        name = format!("{}/{name}", ancestor.to_string_lossy());
        if free(&name) {
            return name;
        }
    }
    (2..)
        .map(|suffix| format!("{name}-{suffix}"))
        .find(|value| free(value))
        .unwrap()
}

pub fn register_paths(state: &mut State, paths: &[PathBuf]) -> Vec<String> {
    let mut leaf_counts = HashMap::new();
    for path in paths {
        *leaf_counts.entry(leaf(path)).or_insert(0) += 1;
    }
    paths
        .iter()
        .map(|path| {
            let name = available_name(state, path, leaf_counts[&leaf(path)] > 1);
            git::warn_collision(&name);
            state.repos.insert(
                name.clone(),
                Repo {
                    path: path.clone(),
                    ..Repo::default()
                },
            );
            name
        })
        .collect()
}

pub fn register(
    state: &mut State,
    args: &ArgMatches,
    scanning: bool,
    jobs: Option<usize>,
) -> Result<(bool, bool)> {
    let mut roots: Vec<PathBuf> = args
        .get_many::<PathBuf>("paths")
        .into_iter()
        .flatten()
        .map(|p| abs(p))
        .collect::<Result<_>>()?;
    if roots.is_empty() {
        roots.push(std::env::current_dir()?);
    }
    let bare = args.get_flag("bare");
    let auto = args.get_flag("auto");
    let skip = args.get_flag("skip");
    let scanning_paths = scanning || auto || args.get_flag("recursive");
    let (repos, missing) = if scanning_paths {
        let depth = scanning
            .then(|| args.get_one::<usize>("depth").copied())
            .flatten();
        let excludes = if scanning {
            words(args, "exclude")
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let result = scan::discover(&roots, depth, &excludes, bare, skip, jobs)?;
        (result.repos, scanning && result.missing)
    } else {
        (
            roots
                .iter()
                .filter(|path| git::is_repo(path, bare, skip))
                .map(|path| (path.clone(), path.clone()))
                .collect(),
            false,
        )
    };
    let existing: HashSet<_> = state
        .repos
        .values()
        .map(|repo| target::path_key(&repo.path))
        .collect();
    let mut seen = HashSet::new();
    let mut new = Vec::new();
    let mut already = 0;
    for (path, _) in &repos {
        let key = target::path_key(path);
        if !seen.insert(key.clone()) {
            continue;
        }
        if existing.contains(&key) {
            already += 1;
        } else {
            new.push(path.clone());
        }
    }
    new.sort();
    if scanning {
        println!(
            "Found {} new repo(s), {already} already registered.",
            new.len()
        );
    } else if new.is_empty() {
        println!("No new repos found!");
    } else {
        println!("Found {} new repo(s).", new.len());
    }
    let group = word(args, "group");
    if let Some(group) = group {
        check_group(state, group, true)?;
    }
    let group_path = args
        .get_one::<PathBuf>("group-path")
        .map(|p| abs(p))
        .transpose()?;
    let names = register_paths(state, &new);
    if let Some(group) = group {
        check_group(state, group, true)?;
    }
    if args.get_flag("dry") {
        for path in new {
            println!("{}", path.display());
        }
        return Ok((false, missing));
    }
    if new.is_empty() {
        return Ok((false, missing));
    }
    if auto {
        let registered: HashMap<_, _> = new
            .iter()
            .zip(&names)
            .map(|(p, n)| (target::path_key(p), n))
            .collect();
        let before = state.groups.len();
        for (path, root) in &repos {
            let Some(&name) = registered.get(&target::path_key(path)) else {
                continue;
            };
            let Some(relative) = path
                .parent()
                .and_then(|parent| parent.strip_prefix(root).ok())
            else {
                continue;
            };
            let mut group_name = leaf(root);
            let mut group_path = root.clone();
            for component in std::iter::once(None).chain(relative.iter().map(Some)) {
                if let Some(component) = component {
                    group_name.push('-');
                    group_name.push_str(&component.to_string_lossy());
                    group_path.push(component);
                }
                if check_group(state, &group_name, true).is_err()
                    || state.groups.get(&group_name).is_some_and(|group| {
                        group
                            .path
                            .as_ref()
                            .is_none_or(|p| target::path_key(p) != target::path_key(&group_path))
                    })
                {
                    eprintln!("Warning: skipping conflicting automatic group {group_name}");
                    continue;
                }
                add_group(state, &group_name, [name.clone()], Some(group_path.clone()));
            }
        }
        println!("Created {} new group(s).", state.groups.len() - before);
    }
    if let Some(group) = group {
        let create = !state.groups.contains_key(group);
        if create {
            println!("{group} does not exists, creating it.");
        }
        println!("Added {} repos to the {group} group", names.len());
        add_group(state, group, names, if create { group_path } else { None });
    }
    Ok((true, missing))
}

fn replace_key<T>(map: &mut indexmap::IndexMap<String, T>, old: &str, new: &str) {
    let (index, _, value) = map.shift_remove_full(old).unwrap();
    map.shift_insert(index, new.to_owned(), value);
}

fn unregister(state: &mut State, names: &[&str]) {
    for name in names {
        state.repos.shift_remove(*name);
    }
    for group in state.groups.values_mut() {
        group.repos.retain(|n| !names.contains(&n.as_str()));
    }
}

fn doctor(state: &mut State, dry: bool) -> Result<bool> {
    let mut missing = Vec::new();
    for (name, repo) in &state.repos {
        if !repo
            .path
            .try_exists()
            .with_context(|| format!("Cannot inspect {name}: {}", repo.path.display()))?
        {
            missing.push(name.clone());
        }
    }
    if missing.is_empty() {
        println!("No missing repository paths.");
        return Ok(false);
    }
    for name in &missing {
        println!("Missing: {name} ({})", state.repos[name].path.display());
    }
    if dry {
        return Ok(false);
    }
    unregister(
        state,
        &missing.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    Ok(true)
}

pub fn state_command(state: &mut State, name: &str, args: &ArgMatches, cwd: &Path) -> Result<bool> {
    match name {
        "ls" => {
            match word(args, "repo") {
                Some(repo) => println!("{}", state.repos[repo].path.display()),
                None => println!(
                    "{}",
                    state
                        .repos
                        .iter()
                        .filter(|(_, r)| r.path.exists())
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            }
            Ok(false)
        }
        "clear" => {
            *state = State::default();
            Ok(true)
        }
        "rm" => {
            unregister(state, &words(args, "repo"));
            Ok(true)
        }
        "doctor" => doctor(state, args.get_flag("dry")),
        "rename" => {
            let old = word(args, "repo").unwrap();
            let new = word(args, "new").unwrap();
            if target::named(state, new) {
                bail!("{new} is already in use!");
            }
            git::warn_collision(new);
            replace_key(&mut state.repos, old, new);
            for group in state.groups.values_mut() {
                for name in &mut group.repos {
                    if name == old {
                        *name = new.to_owned();
                    }
                }
                group.repos.sort();
            }
            Ok(true)
        }
        "group" => group(state, args),
        "context" => {
            if let Some(name) = word(args, "group") {
                state.context = (name != "none").then(|| name.to_owned());
                return Ok(true);
            }
            match target::context(state, cwd) {
                Some(group) => {
                    println!("{group}: {}", target::member_names(state, group).join(" "))
                }
                None if state.context.as_deref() == Some("auto") => {
                    println!("auto: none detected!")
                }
                None => println!("Context is not set"),
            }
            Ok(false)
        }
        "flags" => {
            if let Some(("set", args)) = args.subcommand() {
                state.repos[word(args, "repo").unwrap()].flags = words(args, "values")
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                return Ok(true);
            }
            for (name, repo) in &state.repos {
                if !repo.flags.is_empty() {
                    println!("{name}: {}", repo.flags.join(" "));
                }
            }
            Ok(false)
        }
        _ => unreachable!(),
    }
}

fn group(state: &mut State, args: &ArgMatches) -> Result<bool> {
    match args.subcommand() {
        Some(("ls", _)) => println!(
            "{}",
            state
                .groups
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Some(("add", args)) => {
            let name = word(args, "name").unwrap();
            check_group(state, name, true)?;
            git::warn_collision(name);
            add_group(
                state,
                name,
                words(args, "repo").into_iter().map(str::to_owned),
                args.get_one::<PathBuf>("path")
                    .map(|p| abs(p))
                    .transpose()?,
            );
            return Ok(true);
        }
        Some(("rmrepo", args)) => {
            let name = word(args, "name").unwrap();
            let Some(group) = state.groups.get_mut(name) else {
                bail!("Group {name} does not exist");
            };
            let repos = words(args, "repo");
            group.repos.retain(|r| !repos.contains(&r.as_str()));
            return Ok(true);
        }
        Some(("rename", args)) => {
            let old = word(args, "group").unwrap();
            let new = word(args, "new").unwrap();
            check_group(state, new, false)?;
            git::warn_collision(new);
            replace_key(&mut state.groups, old, new);
            if state.context.as_deref() == Some(old) {
                state.context = Some(new.to_owned());
            }
            return Ok(true);
        }
        Some(("rm", args)) => {
            for name in words(args, "group") {
                state.groups.shift_remove(name);
            }
            return Ok(true);
        }
        sub => {
            if let Some(name) = sub.and_then(|(_, args)| word(args, "group")) {
                println!("{}", target::member_names(state, name).join(" "));
            } else {
                for (name, Group { path, .. }) in &state.groups {
                    println!(
                        "\x1b[4m{name}\x1b[0m: {}",
                        path.as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                    for repo in target::member_names(state, name) {
                        println!("  - {repo}");
                    }
                }
            }
        }
    }
    Ok(false)
}

pub fn settings_command(
    dir: &Path,
    settings: &Settings,
    name: &str,
    args: &ArgMatches,
) -> Result<()> {
    let path = dir.join("settings.json");
    let sub = args.subcommand();
    if sub.is_none_or(|(name, _)| name == "ll") {
        if name == "color" {
            render::show_colors(settings);
        } else {
            let items = settings.items();
            println!("In use: {}", items.join(","));
            let mut unused: Vec<_> = INFO
                .into_iter()
                .filter(|i| !items.iter().any(|s| s == i))
                .collect();
            unused.sort_unstable();
            if !unused.is_empty() {
                println!("Unused: {}", unused.join(","));
            }
        }
        return Ok(());
    }
    let mut data: serde_json::Map<String, Value> = config::read(&path)?;
    match (name, sub.unwrap()) {
        ("color", ("reset", _)) => {
            data.shift_remove("colors");
        }
        ("color", ("set", args)) => {
            let mut colors = settings.colors.clone();
            colors.insert(
                word(args, "situation").unwrap().to_owned(),
                word(args, "color").unwrap().to_owned(),
            );
            data.insert("colors".into(), json!(colors));
        }
        ("info", ("set-length", _)) => {
            data.insert("layout".into(), json!({"branch":19,"symbols":5,"branch_name":27,"commit_msg":0,"commit_time":0,"path":30}));
            println!("Settings are in {}", path.display());
        }
        ("info", (action, args)) => {
            let item = word(args, "item").unwrap();
            let mut items: IndexSet<_> = settings.items().into_iter().collect();
            if action == "add" {
                items.insert(item.to_owned());
            } else {
                items.shift_remove(item);
            }
            data.insert("info_items".into(), json!(items));
        }
        _ => unreachable!(),
    }
    config::save(&path, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_use_enough_ancestors_to_resolve_a_collision() {
        let mut state = State::default();
        let path = Path::new("root/team/app");
        assert_eq!(available_name(&state, path, false), "app");
        assert_eq!(available_name(&state, path, true), "team/app");
        state.groups.insert("app".into(), Group::default());
        state.repos.insert("team/app".into(), Repo::default());
        assert_eq!(available_name(&state, path, false), "root/team/app");
    }
}
