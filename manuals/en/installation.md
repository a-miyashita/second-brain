# Installation

This chapter shows how to install `sb` on your computer.

## Install a release

**Note:** The project has not published releases yet. Until it does, build the tool
from source (see the next section).

On macOS and Linux, run:

```sh
$ curl -LsSf https://github.com/a-miyashita/second-brain/releases/latest/download/second-brain-installer.sh | sh
```

On Windows, run this command in PowerShell:

```powershell
> irm https://github.com/a-miyashita/second-brain/releases/latest/download/second-brain-installer.ps1 | iex
```

On macOS and Linux, you can also use Homebrew:

```sh
$ brew install a-miyashita/tap/second-brain
```

The installer puts `second-brain` and `sb` in a folder and adds that folder to your
`PATH`. Open a new terminal after the installation.

## Build from source

You need Rust 1.88 or later. Then run this command in the repository folder:

```sh
$ cargo install --path crates/sb-cli --locked
```

The command installs `second-brain` and `sb`.

## Check the installation

Run this command:

```sh
$ sb version
```

The command shows the version of the tool, your platform, the skill version and the
database schema version. For example:

```text
second-brain 0.1.0 (x86_64-linux), skill version 2, schema 3
```

If your terminal says that it cannot find `sb`, the folder of the program is not in
your `PATH`. Open a new terminal, or add the folder to your `PATH`.

## Next step

Go to [Getting started](getting-started.md).
