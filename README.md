# tauri-tutorial-runner

Executable, verifiable tutorials for [Tauri](https://tauri.app) apps. A tutorial is
declared as a base scaffold plus deterministic mutations plus programmatic
assertions; one YAML file compiles into a readable tutorial, a generatable template,
and a CI gate that fails when Tauri moves under the docs.

Status: seven tutorials under `tutorials/`: greet-command (commands, the fs
plugin, and capability ACLs), rsbuild (swapping the frontend build tool),
splashscreen, and four plugin installs (store, http, sql, log).
The CLI core is Rust; the original JS implementation lives in the git history.

## Why

Tauri's docs CI never compiles the snippets embedded in its pages, so guides
drift as the framework moves: APIs change, scaffolds change, and readers are the
first to find out. A tutorial that runs is a tutorial that can't silently rot.

## Usage

The CLI binary is `tatu`:

```
cargo run -q -- check tutorials/greet-command      # advisory run on your machine
cargo run -q -- check tutorials/greet-command --step verify-greet   # one step only
cargo run -q -- validate tutorials/rsbuild         # parse + validate only
cargo run -q -- verify tutorials/greet-command     # last run's tree vs expected
cargo run -q -- bless tutorials/greet-command      # accept the last run's tree
cargo run -q -- revendor bases/vanilla-ts@4.7.3    # re-scaffold a pool base with CTA
cargo run -q -- schema                             # regenerate schemas/*.json
cargo run -q -- schema --emit-ts path/to/types.ts  # manifest contract as TypeScript
```

Or `cargo install --path .` and use `tatu` directly.

`tatu check` restores the tutorial's vendored base scaffold into `.tatu/work/`,
applies each step's mutations, generates one Rust integration test per assertion
phase, and runs them with `cargo test`. Host runs are advisory: results depend on
your platform's toolchain and webview stack, and each step compiles a real Tauri
app, so expect the first run to be slow.

`tatu run` is the authoritative mode: it refuses to run outside the pinned
container (`TATU_ALLOW_LOCAL=1` overrides) and writes a tutorial tree under
`.tatu/out/<id>/`, holding a `manifest.json` (per step, the mutated files, shell
commands, and assertion results) alongside the content of every file a mutation
touched. `tatu verify` compares that tree against the `expected/` tree committed
with each tutorial (ignoring the run-environment fields), and `tatu bless`
accepts a reviewed run as the new expected tree.

The `ipc-acl` assertion kind tests real capability ACL behavior headlessly: the
generated tests build the app with `tauri::generate_context!()`, which resolves the
actual `capabilities/*.json` under `cargo test` without any frontend build. A
tutorial step can assert that a plugin command is denied before a permission is
granted and succeeds after — the silent-ACL-failure class, made loud.

## Tutorial anatomy

```
bases/<template>@<version>/  # vendored create-tauri-app scaffolds, shared by tutorials
tutorials/<id>/
  tutorial.yaml            # steps: task (agent prompt) + mutations + assertions;
                           # base.fixture points into bases/
  steps/<step>/            # overlay files, copied over the work tree by that step
  expected/                # committed output of the last authoritative run
    manifest.json          # steps, mutations, and assertion results
    base/<file>            # what a file held before the tutorial first touched it
    steps/<step>/<file>    # what it holds after that step
```

Overlays are the authoring surface; the runner records the content of every
mutated file before and after the step, and consumers derive the diffs readers
see from that pair. The run proves the pair too: a recorded before must match
what the step actually found on disk, so a shell command or an assertion that
rewrites a file between two steps fails the run instead of quietly corrupting
the record. The `bases/` trees are unmodified create-tauri-app output, generated
with the app name `tatu-app` and shared across tutorials; re-vendoring one is an
explicit, reviewed operation, and the runner fails when a re-vendored base
diverges from an overlay outside what the tutorial recorded.

`tatu revendor bases/<template>@<version>` re-scaffolds a pool base: it requires
that exact create-tauri-app version installed (`cargo install
create-tauri-app@<version> --locked` — npm can lag behind crates.io), drops
CTA's own `.gitignore` and `README.md`, regenerates `src-tauri/Cargo.lock`, and
prints what changed. Follow with `tatu check` on every tutorial using the base,
review, then `tatu bless`. Bumping the CTA version means scaffolding into a new
`bases/<template>@<new-version>/` dir and pointing tutorials at it one by one.

## Adding a tutorial

Point `base.fixture` at a scaffold in `bases/` (or vendor a new one with
create-tauri-app, using the app name `tatu-app`), describe the steps in
`tutorial.yaml` (the two existing tutorials show the step shape), and author
each step's file changes as overlays under `steps/<step>/`.
Iterate with `tatu check tutorials/<id>` until green, then run an authoritative
`tatu run` and accept its tree with `tatu bless tutorials/<id>`.

## CI

- `ci` runs on every push and pull request: `cargo fmt`, `clippy -D warnings`,
  `cargo test` (including the byte-compat gate for generated test templates), a
  no-CRLF guard (CRLF in a vendored base poisons the recorded snapshots), and a
  check that the committed schemas match the code.
- `tutorials` runs every tutorial in the pinned container, weekly and on manual
  dispatch, and compares each tree against the committed expected one
  (`tatu verify`). A red step means the tutorial, or Tauri underneath it, broke.
- `image` publishes the container image to GHCR whenever its inputs change,
  tagged with the content hash of those inputs (the `Dockerfile` plus the
  lockfiles whose dep graphs it pre-fetches); `tutorials` pulls that tag and
  builds locally only when it has not been published yet.

## Prerequisites

Building the generated tests means building Tauri, so a host run needs the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.
The rsbuild tutorial's shell steps also need Node and pnpm. The container image
pins Rust at Tauri's MSRV or newer; `rust-version` in `Cargo.toml` covers only the
`tatu` binary itself.

## Windows note

`cargo test` executables that link tauri with default features crash at load with
`STATUS_ENTRYPOINT_NOT_FOUND`, because the comctl32 v6 manifest that Tauri's build
script embeds into app binaries never reaches test executables. The runner injects
the manifest into the work tree automatically (`rustc-link-arg-tests`), which is
also why generated tests are integration-test targets: link args never reach
`#[cfg(test)]` modules inside `src/`.

## License

[Apache License, Version 2.0](LICENSE)
