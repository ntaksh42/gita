use super::{Query, Situation, Status};
use crate::config::Repo;
use anyhow::{Result, bail};
use gix::bstr::{BStr, ByteSlice};
use std::time::{SystemTime, UNIX_EPOCH};

fn text(value: &BStr) -> String {
    value.to_str_lossy().into_owned()
}

pub fn read(item: &Repo, query: Query) -> Result<Status> {
    if !item.flags.is_empty() {
        bail!("repository has Git flags");
    }
    let mut repo = gix::open(&item.path)?;
    if matches!(query, Query::Status { .. }) {
        let mut config = repo.config_snapshot_mut();
        config.set_value(&gix::config::tree::Status::SHOW_UNTRACKED_FILES, "normal")?;
        config.commit()?;
    }
    let cfg = repo.config_snapshot();
    for key in ["core.sparseCheckout", "core.splitIndex"] {
        if cfg.boolean(key).unwrap_or_default() {
            bail!("{key} is enabled");
        }
    }
    if let Some(extensions) = cfg.sections_by_name("extensions") {
        for section in extensions {
            for key in section.body().value_names() {
                if !["objectformat", "worktreeconfig"]
                    .iter()
                    .any(|s| key.eq_ignore_ascii_case(s))
                {
                    bail!("unsupported repository extension: {key}");
                }
            }
        }
    }
    // Git plumbing environment can change the index, worktree, or object store.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
            bail!("{key} is configured");
        }
    }

    let head = repo.head()?;
    let branch_ref = head.referent_name();
    let commit = head
        .id()
        .map(|id| id.object().and_then(|obj| obj.try_into_commit()))
        .transpose()?;
    let branch = match (branch_ref, &commit) {
        (Some(name), _) => text(name.shorten()),
        (None, Some(commit)) => {
            use gix::commit::describe::SelectRef;
            commit
                .describe()
                .names(SelectRef::AllTags)
                .max_candidates(0)
                .try_format()?
                .map(|name| name.to_string())
                .unwrap_or_default()
        }
        _ => String::new(),
    };
    let mut result = Status {
        branch,
        bare: repo.is_bare(),
        ..Status::default()
    };
    use gix::remote::Direction;
    if matches!(query, Query::Freeze) {
        let mut names: Vec<_> = repo.remote_names().into_iter().collect();
        names.sort();
        let remote = repo
            .try_find_remote("origin")
            .or_else(|| {
                names
                    .first()
                    .and_then(|name| repo.try_find_remote(name.as_bstr()))
            })
            .transpose()?;
        result.url = remote.and_then(|remote| {
            remote
                .url(Direction::Fetch)
                .map(|url| text(url.to_bstring().as_bstr()))
        });
        return Ok(result);
    }
    if let (Some(branch), Some(commit)) = (branch_ref, &commit) {
        // A local upstream is expressed as remote='.' and has no tracking refspec.
        let upstream = if cfg
            .string(format!("branch.{}.remote", text(branch.shorten())).as_str())
            .is_some_and(|remote| remote.as_bstr() == b".")
        {
            cfg.string(format!("branch.{}.merge", text(branch.shorten())).as_str())
        } else {
            repo.branch_remote_tracking_ref_name(branch, Direction::Fetch)
                .transpose()?
                .map(|r| r.as_bstr().to_owned())
        };
        if let Some(upstream) = upstream
            && let Some(mut reference) = repo.try_find_reference(upstream.as_bstr())?
        {
            let upstream = reference.peel_to_id()?;
            if upstream == commit.id() {
                result.situation = Situation::InSync;
            } else {
                // Only the signs of the two counts are displayed. Walking stops on the first
                // occurrence of the opposite tip, or when no ancestor can reach it.
                let local_ancestor = reaches(upstream, commit.id)?;
                let remote_ancestor = reaches(commit.id(), upstream.detach())?;
                result.situation =
                    Situation::counts(u64::from(!local_ancestor), u64::from(!remote_ancestor));
            }
        }
    }
    if !result.bare {
        let index = repo.index_or_empty()?;
        if index.is_sparse() || index.link().is_some() {
            bail!("unsupported sparse or split index");
        }
        if index
            .entries()
            .iter()
            .any(|entry| entry.mode == gix::index::entry::Mode::COMMIT)
        {
            bail!("submodule status needs Git");
        }
        use gix::status::{Item, UntrackedFiles, index_worktree::Item as Worktree};
        let configured = repo
            .status(gix::progress::Discard)?
            .untracked_files(UntrackedFiles::Collapsed)
            .index_worktree_options_mut(|options| options.thread_limit = Some(1));
        for item in configured.into_iter([])? {
            match item? {
                Item::TreeIndex(_) => result.staged = true,
                Item::IndexWorktree(Worktree::Modification { entry, .. }) => {
                    result.dirty = true;
                    if entry.stage_raw() != 0 {
                        result.staged = true;
                    }
                }
                Item::IndexWorktree(Worktree::DirectoryContents { .. }) => result.untracked = true,
                Item::IndexWorktree(Worktree::Rewrite { .. }) => {
                    result.dirty = true;
                    result.untracked = true;
                }
            }
        }
        result.stashed = repo.try_find_reference("refs/stash")?.is_some();
    }
    if matches!(query, Query::Status { commit: true })
        && let Some(commit) = commit
    {
        result.message = text(commit.message()?.summary().as_ref());
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        result.time = relative_time(commit.time()?.seconds, now);
    }
    Ok(result)
}

fn reaches(start: gix::Id<'_>, target: gix::ObjectId) -> Result<bool> {
    for commit in start.ancestors().all()? {
        let id = commit?.id;
        if id == target {
            return Ok(true);
        }
    }
    Ok(false)
}

fn relative_time(seconds: i64, now: i64) -> String {
    if seconds > now {
        return "in the future".into();
    }
    let age = now.saturating_sub(seconds) as u64;
    let unit = |n, word| format!("{n} {word}{} ago", if n == 1 { "" } else { "s" });
    if age < 90 {
        return unit(age, "second");
    }
    let minutes = (age + 30) / 60;
    if minutes < 90 {
        return unit(minutes, "minute");
    }
    let hours = (minutes + 30) / 60;
    if hours < 36 {
        return unit(hours, "hour");
    }
    let days = (hours + 12) / 24;
    if days < 14 {
        return unit(days, "day");
    }
    if days < 70 {
        return unit((days + 3) / 7, "week");
    }
    if days < 365 {
        return unit((days + 15) / 30, "month");
    }
    if days < 1825 {
        let months = (days * 12 + 183) / 365;
        let years = months / 12;
        let months = months % 12;
        if months == 0 {
            return unit(years, "year");
        }
        return format!(
            "{years} year{}, {}",
            if years == 1 { "" } else { "s" },
            unit(months, "month")
        );
    }
    unit((days + 183) / 365, "year")
}
