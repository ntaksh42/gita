use crate::{exec, git, target};
use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

pub struct Scan {
    pub repos: Vec<(PathBuf, PathBuf)>,
    pub missing: bool,
}

#[cfg(windows)]
fn is_link(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
}
#[cfg(not(windows))]
fn is_link(path: &Path) -> bool {
    path.is_symlink()
}

pub fn discover(
    roots: &[PathBuf],
    depth: Option<usize>,
    exclude: &[String],
    bare: bool,
    skip: bool,
    jobs: Option<usize>,
) -> Result<Scan> {
    let mut builder = GlobSetBuilder::new();
    for glob in exclude {
        builder.add(Glob::new(glob)?);
    }
    let excludes = builder.build()?;
    let errors = AtomicUsize::new(0);
    let found = AtomicUsize::new(0);
    let progress = io::stderr().is_terminal();
    let result = Mutex::new(Vec::new());
    let mut missing = false;
    for root in roots {
        if !root.exists() {
            eprintln!("{} does not exist", root.display());
            missing = true;
            continue;
        }
        if is_link(root) {
            continue;
        }
        let root_filter = root.clone();
        let excludes = excludes.clone();
        WalkBuilder::new(root)
            .standard_filters(false)
            .follow_links(false)
            .max_depth(depth)
            .threads(jobs.unwrap_or_else(exec::cpu_count))
            .filter_entry(move |entry| {
                let p = entry.path();
                if entry.depth() == 0 {
                    return true;
                }
                !is_link(p)
                    && p.file_name().is_none_or(|n| n != ".git")
                    && !p
                        .file_name()
                        .is_some_and(|n| excludes.is_match(Path::new(n)))
                    && !p
                        .strip_prefix(&root_filter)
                        .is_ok_and(|p| excludes.is_match(p))
            })
            .build_parallel()
            .run(|| {
                Box::new(|entry| {
                    match entry {
                        Ok(entry)
                            if entry.file_type().is_some_and(|kind| kind.is_dir())
                                && git::is_repo(entry.path(), bare, skip) =>
                        {
                            result
                                .lock()
                                .unwrap()
                                .push((entry.into_path(), root.clone()));
                            let count = found.fetch_add(1, Ordering::Relaxed) + 1;
                            if progress {
                                eprint!("\rFound {count} repo(s)");
                                let _ = io::stderr().flush();
                            }
                        }
                        Err(_) => {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    WalkState::Continue
                })
            });
    }
    if progress && found.into_inner() > 0 {
        eprintln!();
    }
    let skipped = errors.into_inner();
    if skipped > 0 {
        eprintln!("Skipped {skipped} unreadable path(s).");
    }
    let mut repos = result.into_inner().unwrap();
    repos.sort();
    repos.dedup_by(|a, b| target::path_key(&a.0) == target::path_key(&b.0) && a.1 == b.1);
    Ok(Scan { repos, missing })
}
