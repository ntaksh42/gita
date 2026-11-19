use crate::{
    CliError,
    config::{Repo, State},
    git,
};
use anyhow::Result;
use indexmap::IndexSet;
use std::path::{Path, PathBuf};

pub fn path_key(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\").to_lowercase())
    } else {
        path.to_owned()
    }
}

pub fn relative_depth(base: &Path, path: &Path) -> Option<usize> {
    path_key(path)
        .strip_prefix(path_key(base))
        .ok()
        .map(|relative| relative.components().count())
}

pub fn context<'a>(state: &'a State, cwd: &Path) -> Option<&'a str> {
    match state.context.as_deref()? {
        "auto" => state
            .groups
            .iter()
            .filter_map(|(name, group)| {
                Some((relative_depth(group.path.as_deref()?, cwd)?, name.as_str()))
            })
            .min_by_key(|(depth, _)| *depth)
            .map(|(_, name)| name),
        name => state.groups.contains_key(name).then_some(name),
    }
}

pub fn member_names<'a>(state: &'a State, group: &str) -> Vec<&'a str> {
    state
        .groups
        .get(group)
        .into_iter()
        .flat_map(|g| &g.repos)
        .filter(|name| state.repos.contains_key(*name))
        .map(String::as_str)
        .collect()
}

pub fn named(state: &State, word: &str) -> bool {
    state.repos.contains_key(word) || state.groups.contains_key(word)
}

pub fn repos<'a>(
    state: &'a State,
    names: &[&str],
    default: Option<&str>,
) -> Vec<(&'a str, &'a Repo)> {
    let mut selected = IndexSet::new();
    for &name in names {
        if let Some((name, _)) = state.repos.get_key_value(name) {
            selected.insert(name.as_str());
        } else {
            selected.extend(member_names(state, name));
        }
    }
    if names.is_empty() {
        if let Some(group) = default {
            selected.extend(member_names(state, group));
        } else {
            selected.extend(state.repos.keys().map(String::as_str));
        }
    }
    selected
        .into_iter()
        .map(|name| (name, &state.repos[name]))
        .collect()
}

pub fn unknown(word: &str) -> CliError {
    CliError(format!("{word} is not a repo or group"))
}

pub fn split<'a>(state: &State, words: &'a [String]) -> Result<(Vec<&'a str>, Vec<&'a str>)> {
    let boundary = words.iter().position(|w| w == "--");
    let mut names = Vec::new();
    let mut remaining = Vec::new();
    let mut mode = false;
    let mut started = false;
    for (index, word) in words.iter().enumerate() {
        if boundary == Some(index) {
            started = true;
            continue;
        }
        if !started {
            if matches!(word.as_str(), "-q" | "--quote-mode") {
                mode = true;
                continue;
            }
            if named(state, word) {
                names.push(word.as_str());
                continue;
            }
            if boundary.is_some() {
                return Err(unknown(word).into());
            }
            started = true;
        }
        remaining.push(word.as_str());
    }
    if remaining.is_empty() {
        let hint = names
            .last()
            .filter(|name| boundary.is_none() && git::is_command(name))
            .map(|name| {
                format!(": {name} was treated as a repo or group. Use -- before the command.")
            })
            .unwrap_or_default();
        return Err(CliError(format!("Missing commands{hint}")).into());
    }
    if mode && remaining.len() != 1 {
        return Err(unknown(remaining[0]).into());
    }
    Ok((names, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Group;

    fn fixture() -> State {
        let mut state = State::default();
        for name in ["a", "b"] {
            state.repos.insert(name.into(), Repo::default());
        }
        state.groups.insert(
            "parent".into(),
            Group {
                path: Some("tree".into()),
                repos: vec!["a".into(), "never".into()],
            },
        );
        state.groups.insert(
            "inner".into(),
            Group {
                path: Some("tree/src".into()),
                repos: vec!["b".into(), "a".into()],
            },
        );
        state.groups.insert(
            "tie".into(),
            Group {
                path: Some("tree/src".into()),
                repos: vec!["a".into()],
            },
        );
        state
    }

    #[test]
    fn auto_context_chooses_the_nearest_group_and_keeps_ties_stable() {
        let mut s = fixture();
        s.context = Some("auto".into());
        assert_eq!(context(&s, Path::new("tree/src/crate")), Some("inner"));
        assert_eq!(context(&s, Path::new("tree/src-other")), Some("parent"));
        assert!(context(&s, Path::new("tree-other")).is_none());
        #[cfg(windows)]
        assert_eq!(context(&s, Path::new("TREE\\SRC\\crate")), Some("inner"));
        s.context = Some("deleted".into());
        assert!(context(&s, Path::new("tree/src")).is_none());
    }

    #[test]
    fn selection_expands_in_order_and_deduplicates() {
        let s = fixture();
        let selected = |names: &[&str], group| {
            repos(&s, names, group)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(&["inner", "parent", "b"], None), ["b", "a"]);
        assert_eq!(selected(&[], Some("parent")), ["a"]);
        assert_eq!(selected(&[], None), ["a", "b"]);
    }

    #[test]
    fn separator_preserves_command_arguments_and_enforces_names() {
        let s = fixture();
        let words = |args: &[&str]| args.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            split(&s, &words(&["a", "--", "b", "--", "-q"])).unwrap(),
            (vec!["a"], vec!["b", "--", "-q"])
        );
        assert_eq!(
            split(&s, &words(&["-q", "b", "one word"])).unwrap(),
            (vec!["b"], vec!["one word"])
        );
        assert!(split(&s, &words(&["missing", "--", "status"])).is_err());
        assert!(split(&s, &words(&["-q", "b", "status", "--short"])).is_err());
        assert!(split(&s, &words(&["a", "--"])).is_err());
    }
}
