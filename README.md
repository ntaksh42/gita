# gita

A Rust CLI for tracking Git repositories and running Git or shell commands across them. Requires Git 2.35 or later.

## Install

On Windows, use the ARM64 or x64 MSI from the [GitHub releases](https://github.com/ntaksh42/gita/releases).
The MSI installs per user in Local AppData and places gita on the user PATH. Open a new terminal after installing.
PowerShell users installing over the Python version can run installer/install.ps1 from this source tree.
It picks an MSI matching the OS and imports the old repository register, groups and context if repos.json does not yet exist.

On macOS and Linux, build and install from this checkout with:

~~~sh
cargo install --path . --locked
~~~

## Use

~~~sh
gita scan ~/src -a
gita context auto
gita ll
gita fetch
gita super -- status --short
gita shell -- echo hello
gita freeze > repos-export.json
gita clone -f repos-export.json -C ~/restored
~~~

Run gita --help for the commands and gita <command> --help for their options.
Generate dynamic completion with gita completion bash, zsh, fish or powershell. Load the output in the corresponding shell.
See the [CLI specification](docs/spec.html) for configuration and status output.

## Develop

~~~sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
~~~

On Windows, installer/build.ps1 builds a release MSI in dist. The optional Target parameter
accepts an installed aarch64-pc-windows-msvc or x86_64-pc-windows-msvc Rust target.
Run installer/test.ps1 on an account without an existing gita MSI to verify install and uninstall.

The command names and behavior follow [nosarthur/gita](https://github.com/nosarthur/gita).
