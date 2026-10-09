# second-brain

second-brain is a command-line tool (`second-brain`, short alias `sb`) that builds a
personal knowledge base for AI agents.

- It collects context from Slack, Google Meet notes, documents and web pages.
- It stores the raw data as files and keeps a catalog in SQLite.
- It summarizes each entry with an LLM and records which model wrote each summary.
- It lets AI agents search the catalog through an agent skill that calls the CLI
  with `--json`.

## Install

You need Rust 1.88 or later and a C compiler (the SQLite library is built from
source). On Windows, install the Visual Studio C++ build tools.

```sh
cargo install second-brain --locked
```

The command installs two programs, `second-brain` and `sb`, in Cargo's bin directory
(`~/.cargo/bin`). Then run:

```sh
sb setup home
sb doctor
```

## Documentation

- [User manual](https://github.com/a-miyashita/second-brain/tree/main/manuals/en)
- [Source code and design documents](https://github.com/a-miyashita/second-brain)
- [Changelog](https://github.com/a-miyashita/second-brain/blob/main/CHANGELOG.md)

## License

MIT
