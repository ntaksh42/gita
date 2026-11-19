mod cli;
mod oxide;

use crate::{
    config::{Backend, Repo},
    exec,
};
use anyhow::Result;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

#[derive(Clone, Copy)]
pub enum Query {
    Status { commit: bool },
    Freeze,
}

#[derive(Default, Debug, PartialEq)]
pub struct Status {
    pub branch: String,
    pub dirty: bool,
    pub staged: bool,
    pub untracked: bool,
    pub stashed: bool,
    pub situation: Situation,
    pub message: String,
    pub time: String,
    pub url: Option<String>,
    pub bare: bool,
}

#[derive(Default, Debug, PartialEq)]
pub enum Situation {
    #[default]
    NoRemote,
    InSync,
    Diverged,
    LocalAhead,
    RemoteAhead,
}

impl Situation {
    pub fn counts(ahead: u64, behind: u64) -> Self {
        match (ahead, behind) {
            (0, 0) => Self::InSync,
            (_, 0) => Self::LocalAhead,
            (0, _) => Self::RemoteAhead,
            _ => Self::Diverged,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::NoRemote => "no_remote",
            Self::InSync => "in_sync",
            Self::Diverged => "diverged",
            Self::LocalAhead => "local_ahead",
            Self::RemoteAhead => "remote_ahead",
        }
    }
}

fn fallback(name: &str, error: &anyhow::Error) {
    if std::env::var("GITA_DEBUG").as_deref() == Ok("1") {
        eprintln!("{name}: falling back to git: {error:#}");
    }
}

pub fn collect(
    repos: &[(&str, &Repo)],
    backend: Backend,
    query: Query,
    jobs: Option<usize>,
    mut done: impl FnMut(usize, Result<Status>),
) {
    let cpu = exec::cpu_count();
    if backend != Backend::Auto || jobs.is_some() || repos.len() < 2 {
        let limit = jobs.unwrap_or(if backend == Backend::Gix {
            cpu
        } else {
            cpu * 4
        });
        exec::parallel(
            repos,
            limit,
            |(name, repo)| {
                if backend != Backend::Git {
                    match oxide::read(repo, query) {
                        ok @ Ok(_) => return ok,
                        err @ Err(_) if backend == Backend::Gix => return err,
                        Err(error) => fallback(name, &error),
                    }
                }
                cli::read(repo, query, jobs != Some(1))
            },
            done,
        );
        return;
    }

    // gix readers hand off unsupported repositories while continuing to read the rest.
    let (handoff, fallback_jobs) = mpsc::channel::<usize>();
    let fallback_jobs = Mutex::new(fallback_jobs);
    let (send, results) = mpsc::channel();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..(cpu * 4).min(repos.len()) {
            let (send, fallback_jobs) = (send.clone(), &fallback_jobs);
            scope.spawn(move || {
                loop {
                    let index = fallback_jobs.lock().unwrap().recv();
                    let Ok(index) = index else { break };
                    let result = cli::read(repos[index].1, query, true);
                    if send.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        for _ in 0..cpu.min(repos.len()) {
            let (send, handoff, next) = (send.clone(), handoff.clone(), &next);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, repo)) = repos.get(index) else {
                        break;
                    };
                    match oxide::read(repo, query) {
                        Ok(status) => {
                            if send.send((index, Ok(status))).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            fallback(name, &error);
                            if handoff.send(index).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
        drop(handoff);
        drop(send);
        for (index, result) in results {
            done(index, result);
        }
    });
}
