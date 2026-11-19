use assert_cmd::Command;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as Process, Output},
};
use tempfile::TempDir;

struct Fixture(TempDir);
impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn home(&self) -> &Path {
        self.0.path()
    }
    fn path(&self, name: &str) -> PathBuf {
        self.home().join(name)
    }
    fn config(&self, name: &str) -> PathBuf {
        self.path("gita").join(name)
    }
    fn gita(&self) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("gita"));
        cmd.env("GITA_PROJECT_HOME", self.home())
            .env_remove("GITA_JOBS")
            .env_remove("GITA_BACKEND")
            .env_remove("COMPLETE")
            .current_dir(self.home());
        cmd
    }
    fn repo(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        path
    }
    fn add(&self, names: &[&str]) {
        self.gita().arg("add").args(names).assert().success();
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.config("repos.json")).unwrap()).unwrap()
    }
    fn settings(&self, data: Value) {
        fs::create_dir_all(self.path("gita")).unwrap();
        fs::write(self.config("settings.json"), data.to_string()).unwrap();
    }
}

fn git(path: &Path, args: &[&str]) -> String {
    let out = Process::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout(out)
}
fn stdout(out: Output) -> String {
    String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n")
}
fn output(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout(out)
}

#[test]
fn management_preserves_order_groups_and_configuration() {
    let f = Fixture::new();
    f.repo("src/two/app");
    f.repo("src/one/app");
    f.repo("src/.hidden");
    f.gita().args(["add", "-a", "-n", "src"]).assert().success();
    assert!(!f.config("repos.json").exists());
    f.gita().args(["add", "-a", "src"]).assert().success();
    insta::assert_snapshot!(output(f.gita().arg("ls")), @".hidden one/app two/app");
    assert_eq!(f.state()["groups"]["src-one"]["repos"], json!(["one/app"]));
    f.gita()
        .args([
            "group", "add", "one/app", "two/app", "-n", "manual", "-p", "src",
        ])
        .assert()
        .success();
    f.gita().args(["context", "auto"]).assert().success();
    assert_eq!(
        output(f.gita().arg("context").current_dir(f.path("src/one"))),
        "src-one: one/app\n"
    );
    f.gita().args(["context", "manual"]).assert().success();
    f.gita()
        .args(["group", "rename", "manual", "selected"])
        .assert()
        .success();
    f.gita().args(["rename", "one/app", "a"]).assert().success();
    assert_eq!(output(f.gita().arg("ls")), ".hidden a two/app\n");
    assert_eq!(output(f.gita().arg("context")), "selected: a two/app\n");
    f.gita()
        .args(["flags", "set", "a", "-c", "core.commentChar=;"])
        .assert()
        .success();
    assert_eq!(
        f.state()["repos"]["a"]["flags"],
        json!(["-c", "core.commentChar=;"])
    );
    f.gita().args(["flags", "set", "a"]).assert().success();
    f.gita()
        .args(["group", "rmrepo", "a", "-n", "selected"])
        .assert()
        .success();
    f.gita().args(["rm", "two/app"]).assert().success();
    assert_eq!(f.state()["context"], Value::Null);
    assert!(f.path("src/two/app/.git").exists());
    f.gita().args(["group", "rm", "src-one"]).assert().success();
    f.gita().arg("clear").assert().success();
    assert_eq!(f.state()["repos"], json!({}));
}

#[test]
fn scan_filters_depth_patterns_git_metadata_and_links() {
    let f = Fixture::new();
    f.repo("root/outer");
    f.repo("root/outer/nested");
    f.repo("root/outer/.git/internals");
    f.repo("root/.hidden");
    f.repo("root/skip");
    f.repo("root/outer/target/example");
    fs::write(f.path("root/.gitignore"), "outer/\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(f.path("root/outer"), f.path("root/linked")).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = format!(
            r#"mklink /J "{}" "{}""#,
            f.path("root/linked").display(),
            f.path("root/outer").display()
        );
        assert!(
            Process::new("cmd")
                .arg("/C")
                .raw_arg(script)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    f.gita()
        .args(["scan", "root", "does-not-exist", "-d", "1", "-x", "skip"])
        .assert()
        .code(1);
    assert_eq!(output(f.gita().arg("ls")), ".hidden outer\n");
    f.gita()
        .args(["scan", "root", "-x", "skip", "-x", "**/target"])
        .assert()
        .success()
        .stdout("Found 1 new repo(s), 2 already registered.\n");
    assert_eq!(output(f.gita().arg("ls")), ".hidden outer nested\n");
    f.gita()
        .args(["scan", "root", "-a", "-g", "bad"])
        .assert()
        .code(2);
}

#[test]
fn settings_commands_and_corrupt_json() {
    let f = Fixture::new();
    f.settings(
        json!({"future":{"key":true},"symbols":{"untracked":"U"},"colors":{"in_sync":"blue"}}),
    );
    f.gita().args(["info", "add", "path"]).assert().success();
    f.gita()
        .args(["info", "rm", "commit_msg"])
        .assert()
        .success();
    assert_eq!(
        output(f.gita().arg("info")),
        "In use: branch,commit_time,path\nUnused: branch_name,commit_msg\n"
    );
    f.gita().args(["info", "set-length"]).assert().success();
    f.gita()
        .args(["color", "set", "no_remote", "b_blue"])
        .assert()
        .success();
    let s: Value = serde_json::from_slice(&fs::read(f.config("settings.json")).unwrap()).unwrap();
    assert_eq!(s["future"], json!({"key":true}));
    assert_eq!(s["layout"]["branch"], 19);
    assert_eq!(s["colors"]["in_sync"], "blue");
    f.gita().args(["color", "reset"]).assert().success();
    fs::write(f.config("settings.json"), "{\n malformed\n}").unwrap();
    let failure = f.gita().arg("ls").assert().code(1);
    let error = String::from_utf8_lossy(&failure.get_output().stderr);
    assert!(error.contains("settings.json") && error.contains("line 2 column"));
    fs::remove_file(f.config("settings.json")).unwrap();
    fs::write(f.config("repos.json"), r#"{"version":2}"#).unwrap();
    f.gita().arg("ls").assert().code(1);
}

#[test]
fn delegates_shell_and_explicit_target_boundaries() {
    let f = Fixture::new();
    f.repo("status");
    f.repo("other");
    f.add(&["status", "other"]);
    f.gita().args(["super", "status"]).assert().code(2);
    f.gita()
        .args(["super", "unknown", "--", "status"])
        .assert()
        .code(2);
    f.gita()
        .args(["super", "status", "--", "status", "--short"])
        .assert()
        .success();
    f.gita()
        .args(["super", "-q", "status", "--", "status", "--short"])
        .assert()
        .code(2);
    let captured = output(f.gita().args(["shell", "--", "echo", "hello"]));
    assert!(captured.contains("other: hello") && captured.contains("status: hello"));
    f.gita()
        .args(["shell", "other", "--", "exit", "9"])
        .assert()
        .code(1);
    f.gita().args(["br"]).assert().code(2);
    f.gita().args(["st", "missing"]).assert().code(2);
    f.gita()
        .args(["group", "add", "status", "-n", "selected"])
        .assert()
        .success();
    f.gita().args(["context", "selected"]).assert().success();
    let delegated = output(f.gita().arg("br"));
    assert!(delegated.contains(f.path("status").to_str().unwrap()));
    f.settings(json!({"commands":{"custom":{
        "cmd":["git","rev-parse","--show-toplevel"],"allow_all":true,
        "help":"show top level"
    }, "add":{"cmd":"git status"}}}));
    let help = output(f.gita().arg("--help"));
    assert!(help.contains("show top level"));
    f.gita().arg("custom").assert().success();
    f.gita()
        .args(["completion", "powershell"])
        .assert()
        .success();
    let dynamic = output(
        f.gita()
            .env("COMPLETE", "bash")
            .args(["--", "gita", "ls", "st"])
            .env("_CLAP_COMPLETE_INDEX", "2"),
    );
    assert!(dynamic.contains("status"), "{dynamic}");
}

fn ll(f: &Fixture, backend: &str) -> String {
    output(f.gita().args(["ll", "-C"]).env("GITA_BACKEND", backend))
}
fn matches_status(f: &Fixture, symbols: &str) {
    let gix = ll(f, "gix");
    let cli = ll(f, "git");
    assert_eq!(gix, cli);
    assert!(gix.contains(symbols), "expected {symbols:?} in {gix:?}");
}

#[test]
fn backends_agree_on_upstreams_and_worktree_markers() {
    let f = Fixture::new();
    let repo = f.repo("work");
    f.add(&["work"]);
    f.settings(json!({"info_items":["branch"]}));
    matches_status(&f, "[∅]");
    fs::write(repo.join("file"), "base\n").unwrap();
    git(&repo, &["add", "file"]);
    matches_status(&f, "[+∅]");
    git(&repo, &["commit", "-qm", "subject"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &repo,
        &["remote", "add", "origin", "https://example.invalid/example"],
    );
    git(&repo, &["branch", "--set-upstream-to=origin/main"]);
    matches_status(&f, "[]");
    let common = git(&repo, &["rev-parse", "HEAD"]);
    let common = common.trim();
    git(&repo, &["commit", "-qm", "local", "--allow-empty"]);
    let local = git(&repo, &["rev-parse", "HEAD"]);
    matches_status(&f, "[↑]");
    git(&repo, &["reset", "--hard", common]);
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/main", local.trim()],
    );
    matches_status(&f, "[↓]");
    git(&repo, &["commit", "-qm", "other", "--allow-empty"]);
    matches_status(&f, "[⇕]");
    fs::write(repo.join("file"), "stash\n").unwrap();
    git(&repo, &["stash", "push", "-qm", "changes"]);
    fs::write(repo.join("file"), "staged\n").unwrap();
    git(&repo, &["add", "file"]);
    fs::write(repo.join("file"), "dirty\n").unwrap();
    fs::write(repo.join("new"), "untracked\n").unwrap();
    matches_status(&f, "[*+$?⇕]");
    git(&repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
    matches_status(&f, "[*+$?∅]");
    git(&repo, &["tag", "test-tag"]);
    git(&repo, &["checkout", "--detach", "-q"]);
    assert!(ll(&f, "git").contains("test-tag"));
    matches_status(&f, "[*+$?∅]");
}

#[test]
fn simple_gix_status_does_not_need_git_on_path_and_reads_commit() {
    let f = Fixture::new();
    let repo = f.repo("working");
    git(&repo, &["commit", "-qm", "subject", "--allow-empty"]);
    git(&repo, &["update-ref", "refs/tags/tag", "HEAD"]);
    git(&repo, &["checkout", "-q", "--detach"]);
    f.add(&["working"]);
    let out = output(
        f.gita()
            .args(["ll", "-C"])
            .env("PATH", "")
            .env("GITA_BACKEND", "gix"),
    );
    assert!(
        out.contains("tag") && out.contains("subject") && out.contains("ago)"),
        "{out}"
    );
}

#[test]
fn backend_fallback_handles_flags_sparse_and_read_errors() {
    let f = Fixture::new();
    let repo = f.repo("work");
    git(&repo, &["commit", "-qm", "subject", "--allow-empty"]);
    f.add(&["work"]);
    f.settings(json!({"info_items":["branch","commit_msg"]}));
    f.gita()
        .args(["flags", "set", "work", "-c", "core.quotepath=false"])
        .assert()
        .success();
    let explicit = ll(&f, "git");
    assert_eq!(ll(&f, "auto"), explicit);
    let fallback = f
        .gita()
        .args(["ll", "-C"])
        .env("GITA_DEBUG", "1")
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&fallback.get_output().stderr)
            .contains("work: falling back to git")
    );
    f.gita()
        .args(["ll", "-C"])
        .env("GITA_BACKEND", "gix")
        .assert()
        .code(1);
    f.gita().args(["flags", "set", "work"]).assert().success();
    git(&repo, &["config", "core.sparseCheckout", "true"]);
    assert_eq!(ll(&f, "git"), ll(&f, "auto"));
    f.gita()
        .args(["ll", "-C"])
        .env("GITA_BACKEND", "gix")
        .assert()
        .code(1);
    fs::write(repo.join(".git/HEAD"), "invalid").unwrap();
    let error = f
        .gita()
        .args(["ll", "-C"])
        .env("GITA_DEBUG", "1")
        .assert()
        .code(1);
    assert!(
        String::from_utf8_lossy(&error.get_output().stderr).contains("work: falling back to git")
    );
}

#[test]
fn freeze_and_clone_restore_repos_groups_and_branch() {
    let f = Fixture::new();
    let source = f.repo("source");
    git(&source, &["commit", "-qm", "initial", "--allow-empty"]);
    git(&source, &["branch", "topic"]);
    let source = source.to_str().unwrap();
    f.gita()
        .args(["clone", source, "-C", "copy", "-g", "move"])
        .assert()
        .success();
    let copy = f.path("copy/source");
    git(&copy, &["checkout", "-q", "topic"]);
    f.gita()
        .args(["rename", "source", "short"])
        .assert()
        .success();
    f.gita().args(["context", "move"]).assert().success();
    let frozen = output(f.gita().arg("freeze").env("GITA_BACKEND", "gix"));
    assert_eq!(
        frozen,
        output(f.gita().arg("freeze").env("GITA_BACKEND", "git"))
    );
    let archive: Value = serde_json::from_str(&frozen).unwrap();
    assert_eq!(archive["repos"][0]["name"], "short");
    assert_eq!(archive["repos"][0]["branch"], "topic");
    assert_eq!(archive["groups"]["move"]["repos"], json!(["short"]));
    fs::write(f.path("archive.json"), frozen).unwrap();
    f.gita()
        .args(["clone", "-f", "archive.json", "-C", "restore", "-n"])
        .assert()
        .success();
    assert!(!f.path("restore").exists());
    f.gita()
        .args(["clone", "-f", "archive.json", "-C", "restore"])
        .assert()
        .success();
    assert_eq!(
        git(&f.path("restore/source"), &["branch", "--show-current"]),
        "topic\n"
    );
    assert_eq!(
        f.state()["groups"]["move"]["repos"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn simultaneous_registrations_are_atomic() {
    let f = Fixture::new();
    for name in ["one", "two", "three", "four"] {
        f.repo(name);
    }
    let mut children: Vec<_> = ["one", "two", "three", "four"]
        .into_iter()
        .map(|name| {
            Process::new(assert_cmd::cargo::cargo_bin!("gita"))
                .args(["add", name])
                .env("GITA_PROJECT_HOME", f.home())
                .current_dir(f.home())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(f.state()["repos"].as_object().unwrap().len(), 4);
}

#[test]
fn a_new_repo_cannot_share_its_name_with_an_explicit_group() {
    let f = Fixture::new();
    f.repo("example");
    for cmd in ["add", "scan"] {
        f.gita()
            .args([cmd, "example", "-g", "example"])
            .assert()
            .code(1);
        assert!(!f.config("repos.json").exists());
    }
    f.gita()
        .args([
            "clone",
            f.path("example").to_str().unwrap(),
            "-C",
            "copy",
            "-g",
            "example",
        ])
        .assert()
        .code(1);
    assert!(!f.path("copy/example").exists());
}

#[test]
fn bare_and_linked_worktrees_match_in_both_backends() {
    let f = Fixture::new();
    let work = f.repo("work");
    git(&work, &["commit", "-qm", "base", "--allow-empty"]);
    git(&work, &["worktree", "add", "-qb", "linked", "../linked"]);
    git(f.home(), &["clone", "--bare", "work", "mirror.git"]);
    f.gita()
        .args(["add", "-b", "mirror.git", "linked"])
        .assert()
        .success();
    f.settings(json!({"info_items":["branch", "commit_msg"]}));
    fs::write(f.path("linked/new-file"), "example").unwrap();
    assert_eq!(ll(&f, "git"), ll(&f, "gix"));
    assert!(ll(&f, "gix").contains("[?∅]"));
}
