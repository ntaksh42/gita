use crate::config::{CmdDef, INFO, State};
use clap::{Arg, ArgAction, Command, builder::PossibleValuesParser, value_parser};
use indexmap::IndexMap;
use std::{num::NonZeroUsize, path::PathBuf};

fn cmd(name: &'static str, about: &'static str) -> Command {
    Command::new(name)
        .about(about)
        .disable_help_subcommand(true)
}
fn opt(id: &'static str, short: char, long: &'static str) -> Arg {
    Arg::new(id).short(short).long(long)
}
fn flag(id: &'static str, short: char, long: &'static str) -> Arg {
    opt(id, short, long).action(ArgAction::SetTrue)
}
fn path(arg: Arg) -> Arg {
    arg.value_parser(value_parser!(PathBuf))
}
fn choices(arg: Arg, values: impl IntoIterator<Item = String>) -> Arg {
    arg.value_parser(PossibleValuesParser::new(values))
}
fn required(arg: Arg) -> Arg {
    arg.required(true)
}
fn many(arg: Arg) -> Arg {
    arg.num_args(1..).action(ArgAction::Append)
}

fn raw(command: Command) -> Command {
    command.arg(
        Arg::new("args")
            .num_args(0..)
            .allow_hyphen_values(true)
            .trailing_var_arg(true),
    )
}

pub const BUILTINS: &[&str] = &[
    "add",
    "scan",
    "rm",
    "doctor",
    "rename",
    "ls",
    "ll",
    "clear",
    "group",
    "context",
    "flags",
    "color",
    "info",
    "super",
    "shell",
    "freeze",
    "clone",
    "completion",
];

pub fn build(state: &State, delegates: &IndexMap<String, CmdDef>) -> Command {
    let repos = || choices(Arg::new("repo"), state.repos.keys().cloned());
    let groups = || choices(Arg::new("group"), state.groups.keys().cloned());
    let paths = || many(path(Arg::new("paths")));
    let register = |c: Command| {
        c.arg(flag("dry", 'n', "dry-run"))
            .arg(opt("group", 'g', "group"))
            .arg(path(
                Arg::new("group-path").long("group-path").requires("group"),
            ))
            .arg(flag("auto", 'a', "auto-group"))
            .arg(flag("bare", 'b', "bare"))
            .arg(flag("skip", 's', "skip-submodule"))
    };
    let mut app = cmd("gita", "Manage multiple Git repositories")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .arg(flag("version", 'v', "version").exclusive(true))
        .arg(
            opt("jobs", 'j', "jobs")
                .value_parser(value_parser!(NonZeroUsize))
                .value_name("N"),
        )
        .subcommand(
            register(cmd("add", "Register repositories"))
                .arg(required(paths()))
                .arg(flag("recursive", 'r', "recursive").conflicts_with_all(["auto", "bare"]))
                .mut_arg("auto", |a| a.conflicts_with("bare")),
        )
        .subcommand(
            register(cmd("scan", "Discover and register repositories"))
                .mut_arg("group", |arg| arg.conflicts_with("auto"))
                .arg(paths())
                .arg(opt("depth", 'd', "depth").value_parser(value_parser!(usize)))
                .arg(opt("exclude", 'x', "exclude").action(ArgAction::Append)),
        )
        .subcommand(cmd("rm", "Unregister repositories").arg(required(many(repos()))))
        .subcommand(
            cmd(
                "doctor",
                "Unregister repositories whose paths no longer exist",
            )
            .arg(
                flag("dry", 'n', "dry-run")
                    .help("List missing registrations without removing them"),
            ),
        )
        .subcommand(
            cmd("rename", "Rename a repository")
                .arg(required(repos()))
                .arg(required(Arg::new("new"))),
        )
        .subcommand(cmd("ls", "List repository names or print a repository path").arg(repos()))
        .subcommand(cmd("clear", "Clear repositories, groups and context"))
        .subcommand(
            cmd("ll", "Show repository status")
                .arg(groups())
                .arg(flag("no-colors", 'C', "no-colors"))
                .arg(flag("grouped", 'g', "grouped")),
        )
        .subcommand(
            cmd("group", "Manage repository groups")
                .subcommand(cmd("ll", "Show groups").arg(groups()))
                .subcommand(cmd("ls", "List group names"))
                .subcommand(
                    cmd("add", "Add members")
                        .arg(required(many(repos())))
                        .arg(required(opt("name", 'n', "name")))
                        .arg(path(opt("path", 'p', "path"))),
                )
                .subcommand(
                    cmd("rmrepo", "Remove members")
                        .arg(required(many(repos())))
                        .arg(required(opt("name", 'n', "name"))),
                )
                .subcommand(
                    cmd("rename", "Rename a group")
                        .arg(required(groups()))
                        .arg(required(Arg::new("new"))),
                )
                .subcommand(cmd("rm", "Remove groups").arg(required(many(groups())))),
        )
        .subcommand(
            cmd("context", "Show or set the current context").arg(choices(
                Arg::new("group"),
                state
                    .groups
                    .keys()
                    .cloned()
                    .chain(["auto", "none"].map(str::to_owned)),
            )),
        )
        .subcommand(
            cmd("flags", "Manage per-repository Git options")
                .subcommand(cmd("ll", "Show all flags"))
                .subcommand(
                    cmd("set", "Replace flags").arg(required(repos())).arg(
                        Arg::new("values")
                            .num_args(0..)
                            .allow_hyphen_values(true)
                            .trailing_var_arg(true),
                    ),
                ),
        )
        .subcommand(
            cmd("color", "Manage status colors")
                .subcommand(cmd("ll", "Show colors"))
                .subcommand(
                    cmd("set", "Set a status color")
                        .arg(required(choices(
                            Arg::new("situation"),
                            crate::render::SITUATIONS.map(str::to_owned),
                        )))
                        .arg(required(choices(
                            Arg::new("color"),
                            crate::render::color_names(),
                        ))),
                )
                .subcommand(cmd("reset", "Restore default colors")),
        )
        .subcommand(
            cmd("info", "Manage status fields")
                .subcommand(cmd("ll", "Show fields"))
                .subcommand(
                    cmd("add", "Show a field")
                        .arg(required(choices(Arg::new("item"), INFO.map(str::to_owned)))),
                )
                .subcommand(
                    cmd("rm", "Hide a field")
                        .arg(required(choices(Arg::new("item"), INFO.map(str::to_owned)))),
                )
                .subcommand(cmd("set-length", "Set the default column widths")),
        )
        .subcommand(raw(cmd("super", "Run Git in selected repositories")
            .override_usage(
                "gita super [repo|group]... [--] <git-args...> [-q]",
            )))
        .subcommand(raw(cmd(
            "shell",
            "Run a shell command in selected repositories",
        )
        .override_usage("gita shell [repo|group]... [--] <cmd...> [-q]")))
        .subcommand(cmd("freeze", "Export repositories to JSON").arg(choices(
            opt("group", 'g', "group"),
            state.groups.keys().cloned(),
        )))
        .subcommand(
            cmd("clone", "Clone and register repositories")
                .arg(required(Arg::new("source")))
                .arg(path(opt("directory", 'C', "directory")))
                .arg(flag("dry", 'n', "dry-run"))
                .arg(flag("paths", 'p', "paths").requires("file"))
                .arg(opt("group", 'g', "group").conflicts_with("file"))
                .arg(flag("file", 'f', "file")),
        )
        .subcommand(
            cmd("completion", "Generate dynamic shell completion").arg(required(choices(
                Arg::new("shell"),
                ["bash", "zsh", "fish", "powershell"].map(str::to_owned),
            ))),
        );
    for (name, def) in delegates {
        let help = def.help.clone().unwrap_or_else(|| match &def.cmd {
            crate::config::CmdSpec::Line(line) => line.clone(),
            crate::config::CmdSpec::Argv(args) => args.join(" "),
        });
        let usage = if def.allow_all {
            "[repo|group]..."
        } else {
            "<repo|group>..."
        };
        app = app.subcommand(
            Command::new(name.clone())
                .about(help)
                .override_usage(format!("gita {name} {usage} [-s|--shell]"))
                .arg(flag("shell", 's', "shell").hide(true))
                .arg(many(choices(
                    Arg::new("repos"),
                    state.repos.keys().chain(state.groups.keys()).cloned(),
                ))),
        );
    }
    // Dynamic hints for super and shell do not restrict their forwarded arguments.
    for name in ["super", "shell"] {
        use clap_complete::engine::ArgValueCandidates;
        let names: Vec<_> = state
            .repos
            .keys()
            .chain(state.groups.keys())
            .cloned()
            .collect();
        app = app.mut_subcommand(name, |c| {
            c.mut_arg("args", |arg| {
                arg.add(ArgValueCandidates::new(move || {
                    names
                        .iter()
                        .map(|n| clap_complete::engine::CompletionCandidate::new(n.as_str()))
                        .collect()
                }))
            })
        });
    }
    app
}
