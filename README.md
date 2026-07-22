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
omo --help                          # show usage and available subcommands
omo --version                       # print the version
omo init [path]                     # create a new omoplata repository (defaults to .)
omo status [path]                   # show repository status (defaults to .)
omo hash-object [--repo DIR] <path> # store a file as a blob, print its sha256: id (- reads stdin)
omo cat-object [--repo DIR] <id>    # print a stored object: blob bytes, or a tree listing
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

## Object store

Objects are content-addressed by a hash-agile `ObjectId` (`<alg>:<hex>`, SHA-256 in v1) computed over a canonical, self-describing serialization. They are persisted as loose files under `.omoplata/objects/<alg>/<xx>/<rest>`, and every read verifies that the stored bytes still hash to the requested id. Two object kinds exist today — **blobs** (opaque bytes) and **trees** (sorted, name-addressed entries pointing at blobs or subtrees) — with more kinds to come. See [`docs/adr/0002-loose-object-store.md`](docs/adr/0002-loose-object-store.md) for the storage decision.

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
