# omoplata

A version control system built on a Verus-verified merge kernel — *no silent wrong answers*. Definition-level (not file-level), bi-temporal, and git-interoperable.

> **Early scaffold.** This repository currently contains the `omo` CLI and the Cargo workspace skeleton. The verified merge kernel, patch algebra, definition graph, and git interop described in [`Omoplata_design_doc.md`](Omoplata_design_doc.md) are **not yet implemented**.

## Install

Build the release binary (lands at `target/release/omo`):

```sh
cargo build --release
```

Or install the `omo` binary onto your `PATH`:

```sh
cargo install --path crates/omoplata-cli
```

## Usage

```
omo --help              # show usage and available subcommands
omo --version           # print the version
omo init [path]         # create a new omoplata repository (defaults to .)
omo status [path]       # show repository status (defaults to .)
```

Example:

```sh
omo init myrepo
# Initialized empty omoplata repository in myrepo/.omoplata

omo status myrepo
# On omoplata repository at myrepo
# No working changes tracked yet (scaffold).
```

`omo init` creates a `.omoplata/` control directory containing `objects/`, `refs/`, and a `config` file.

## Layout

A Cargo workspace named `omoplata`:

- **`omoplata-cli`** — the `omo` binary (argument parsing and command dispatch).
- **`omoplata-store`** — the on-disk `.omoplata/` control directory and repository handle.

This is deliberately minimal, designed to grow toward the 8-crate decomposition in the design doc (store, algebra, kernel, git interop, and the semantic layer).

## Development

```sh
cargo test --all        # run all tests
cargo fmt               # format (rustfmt is the canonical formatter)
cargo clippy            # lint
```

## Design

See [`Omoplata_design_doc.md`](Omoplata_design_doc.md) for the full design.
