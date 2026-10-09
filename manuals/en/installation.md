# Installation

This chapter shows how to install `sb` on your computer.

## Before you start

You need two things:

- **Rust 1.88 or later.** Install it with [rustup](https://rustup.rs/).
- **A C compiler.** The tool builds its SQLite library from source.
  - Windows: the Visual Studio Build Tools with the "Desktop development with C++"
    workload.
  - macOS: the Xcode command line tools (`xcode-select --install`).
  - Linux: `cc`, for example the package `build-essential`.

## Install the tool

Run this command:

```sh
$ cargo install second-brain --locked
```

The command builds the tool and installs two programs, `second-brain` and `sb`. The
first build takes a few minutes. Cargo puts the programs in `~/.cargo/bin` (on
Windows, `%USERPROFILE%\.cargo\bin`). The Rust installer adds this folder to your
`PATH`. Open a new terminal after the installation.

**Note:** The first release is not published yet. Until it is, build the tool from
source (see "Build from source" below).

## Update the tool

Run the same command with `--force`:

```sh
$ cargo install second-brain --locked --force
```

Then run `sb doctor`. It tells you if the installed agent skill is older than the
program. In that case, run `sb setup skills` again.

## Build from source

Use this method to try a version that is not released yet. Run this command in the
repository folder:

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
your `PATH`. Open a new terminal, or add Cargo's bin folder (`~/.cargo/bin`) to your
`PATH`.

## Next step

Go to [Getting started](getting-started.md).
