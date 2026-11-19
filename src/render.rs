use crate::{
    config::{Repo, Settings, State},
    status::{self, Query, Status},
    target,
};
use anyhow::{Result, bail};
use indexmap::IndexMap;
use std::{
    io::{self, Write},
    path::Path,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SITUATIONS: [&str; 5] = [
    "no_remote",
    "in_sync",
    "diverged",
    "local_ahead",
    "remote_ahead",
];
const COLORS: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "purple", "cyan", "white",
];
const DEFAULT_COLORS: [&str; 5] = ["white", "green", "red", "purple", "yellow"];
const DEFAULT_SYMBOLS: [&str; 5] = ["∅", "", "⇕", "↑", "↓"];

pub fn color_names() -> Vec<String> {
    COLORS
        .map(str::to_owned)
        .into_iter()
        .chain(COLORS.map(|c| format!("b_{c}")))
        .chain(["end", "underline"].map(str::to_owned))
        .collect()
}

fn paint(value: &str, color: &str, enabled: bool) -> String {
    if !enabled {
        return value.to_owned();
    }
    let color_code = |name| {
        COLORS
            .iter()
            .position(|&c| c == name)
            .map(|i| (30 + i).to_string())
    };
    let code = match color {
        "end" => Some("0".into()),
        "underline" => Some("4".into()),
        _ => {
            if let Some(bold) = color.strip_prefix("b_") {
                color_code(bold).map(|code| format!("{code};1"))
            } else {
                color_code(color)
            }
        }
    };
    code.map_or_else(
        || value.to_owned(),
        |code| format!("\x1b[{code}m{value}\x1b[0m"),
    )
}

fn color(settings: &Settings, index: usize) -> &str {
    settings
        .colors
        .get(SITUATIONS[index])
        .map_or(DEFAULT_COLORS[index], String::as_str)
}

pub fn show_colors(settings: &Settings) {
    for row in color_names().chunks(9) {
        println!(
            "{}",
            row.iter()
                .map(|c| paint(c, c, true))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    for (index, key) in SITUATIONS.iter().enumerate() {
        let current = color(settings, index);
        println!("{key}: {}", paint(current, current, true));
    }
}

fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_owned();
    }
    let width = width.max(3);
    if text.width() <= width {
        return pad(text, width);
    }
    let mut prefix = String::new();
    let mut used = 0;
    for ch in text.chars() {
        used += ch.width().unwrap_or(0);
        if used > width - 3 {
            break;
        }
        prefix.push(ch);
    }
    prefix.push_str("...");
    pad(&prefix, width)
}

fn fields(
    repo: &Repo,
    status: &Status,
    settings: &Settings,
    items: &[String],
    colored: bool,
) -> String {
    let index = SITUATIONS
        .iter()
        .position(|&key| key == status.situation.key())
        .unwrap();
    let width = |key: &str| settings.layout.get(key).copied().unwrap_or(0);
    let symbol = |key: &str, default: &'static str| {
        settings.symbols.get(key).map_or(default, String::as_str)
    };
    items
        .iter()
        .map(|item| match item.as_str() {
            "branch" => {
                let flags = if status.bare {
                    String::new()
                } else {
                    let symbols = [
                        (status.dirty, symbol("dirty", "*")),
                        (status.staged, symbol("staged", "+")),
                        (status.stashed, symbol("stashed", "$")),
                        (status.untracked, symbol("untracked", "?")),
                        (true, symbol(SITUATIONS[index], DEFAULT_SYMBOLS[index])),
                    ]
                    .into_iter()
                    .filter_map(|(set, sym)| set.then_some(sym))
                    .collect::<String>();
                    format!("[{symbols}]")
                };
                let info = format!(
                    "{} {}",
                    pad(&status.branch, 10),
                    fit(&flags, width("symbols"))
                );
                paint(
                    &fit(&pad(&info, 18), width(item)),
                    color(settings, index),
                    colored,
                )
            }
            "branch_name" => fit(&status.branch, width(item)),
            "commit_msg" => fit(&status.message, width(item)),
            "commit_time" => {
                if status.time.is_empty() {
                    fit("", width(item))
                } else {
                    fit(&format!("({})", status.time), width(item))
                }
            }
            "path" => paint(
                &fit(&repo.path.to_string_lossy(), width(item)),
                "cyan",
                colored,
            ),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn list(
    state: &State,
    settings: &Settings,
    group: Option<&str>,
    cwd: &Path,
    grouped: bool,
    colored: bool,
    jobs: Option<usize>,
) -> Result<()> {
    let resolved = group.or_else(|| target::context(state, cwd));
    let groups: Vec<Option<&str>> = if grouped && group.is_none() {
        state
            .groups
            .keys()
            .map(|name| Some(name.as_str()))
            .collect()
    } else {
        vec![resolved]
    };
    let mut tasks: IndexMap<&str, &Repo> = IndexMap::new();
    let mut rows = Vec::new();
    for group in groups {
        if grouped && let Some(group) = group {
            rows.push(Row::Heading(group));
        }
        let mut repos = target::repos(state, &[], group);
        repos.sort_by_key(|(name, _)| *name);
        let width = repos
            .iter()
            .map(|(name, _)| name.width())
            .max()
            .unwrap_or(0)
            + 1;
        for (name, repo) in repos {
            let task = if repo.path.is_dir() {
                let (index, _) = tasks.insert_full(name, repo);
                Some(index)
            } else {
                None
            };
            rows.push(Row::Repo {
                name,
                repo,
                width,
                task,
            });
        }
    }
    let tasks: Vec<_> = tasks.into_iter().collect();
    let mut ready: Vec<Option<Result<Status>>> = (0..tasks.len()).map(|_| None).collect();
    let mut rows = rows.iter().peekable();
    let mut failed = false;
    let items = settings.items();
    let commit = items
        .iter()
        .any(|item| matches!(item.as_str(), "commit_msg" | "commit_time"));
    let mut flush = |ready: &[Option<Result<Status>>]| -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        while let Some(row) = rows.peek() {
            match row {
                Row::Heading(name) => writeln!(stdout, "{name}:")?,
                Row::Repo {
                    name,
                    repo,
                    width,
                    task,
                } => {
                    let detail = match task {
                        None => "(missing)".to_owned(),
                        Some(task) => match &ready[*task] {
                            None => break,
                            Some(Ok(status)) => fields(repo, status, settings, &items, colored),
                            Some(Err(error)) => {
                                failed = true;
                                format!("(error: {error:#})")
                            }
                        },
                    };
                    let indent = if grouped { "   " } else { "" };
                    writeln!(stdout, "{indent}{}{detail}", pad(name, *width))?;
                }
            }
            rows.next();
        }
        stdout.flush()
    };
    let backend = settings.backend()?;
    flush(&ready)?;
    let mut error = None;
    status::collect(
        &tasks,
        backend,
        Query::Status { commit },
        jobs,
        |index, status| {
            ready[index] = Some(status);
            if error.is_none() {
                error = flush(&ready).err();
            }
        },
    );
    if let Some(error) = error {
        return Err(error.into());
    }
    if failed {
        bail!("Failed to read one or more repositories");
    }
    Ok(())
}

enum Row<'a> {
    Heading(&'a str),
    Repo {
        name: &'a str,
        repo: &'a Repo,
        width: usize,
        task: Option<usize>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncate_uses_terminal_columns_and_preserves_unicode() {
        assert_eq!(fit("日本語です", 7), "日本...");
        assert_eq!(fit("日本語です", 6), "日... ");
        assert_eq!(fit("日本語", 2), "...");
        assert_eq!(fit("日本語", 0), "日本語");
        assert_eq!(fit("日", 4), "日  ");
        assert_eq!(fit("e\u{301}", 3), "e\u{301}  ");
    }
}
