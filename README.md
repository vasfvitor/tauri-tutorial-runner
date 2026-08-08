# tauri-tutorial-runner

Executable, verifiable tutorials for [Tauri](https://tauri.app) apps. A tutorial is
declared as a base scaffold plus deterministic mutations plus programmatic
assertions; one YAML file compiles into a readable tutorial, a generatable template,
and a CI gate that fails when Tauri moves under the docs.

Status: v0, two tutorials: `tutorials/greet-command` (commands, the fs plugin, and
capability ACLs) and `tutorials/rsbuild` (swapping the frontend build tool).
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
container (`TATU_ALLOW_LOCAL=1` overrides) and writes `tutorial.manifest.json` —
per step, the applied diffs, shell commands, and assertion results. CI compares
that output against the `expected.manifest.json` committed with each tutorial.

The `ipc-acl` assertion kind tests real capability ACL behavior headlessly: the
generated tests build the app with `tauri::generate_context!()`, which resolves the
actual `capabilities/*.json` under `cargo test` without any frontend build. A
tutorial step can assert that a plugin command is denied before a permission is
granted and succeeds after — the silent-ACL-failure class, made loud.

## Tutorial anatomy

```
tutorials/<id>/
  tutorial.yaml            # steps: task (agent prompt) + mutations + assertions
  base/                    # vendored scaffold (create-tauri-app output, committed)
  steps/<step>/            # overlay files, copied over the work tree by that step
  expected.manifest.json   # committed output of the last authoritative run
```

Overlays are the authoring surface; the runner derives base-relative diffs from
them as the canonical record (a raw overlay would silently revert scaffold changes
it doesn't discuss; a diff fails loudly). The `base/` trees are unmodified
create-tauri-app output; re-vendoring one is an explicit, reviewed operation, and
the runner fails when a re-vendored base diverges from an overlay outside its
recorded diff.

## Adding a tutorial

Scaffold a base with create-tauri-app and commit it under `tutorials/<id>/base/`,
describe the steps in `tutorial.yaml` (the two existing tutorials show the step
shape), and author each step's file changes as overlays under `steps/<step>/`.
Iterate with `tatu check tutorials/<id>` until green, then run an authoritative
`tatu run` and commit `.tatu/out/<id>/tutorial.manifest.json` as the tutorial's
`expected.manifest.json`.

## CI

- `ci` runs on every push and pull request: `cargo fmt`, `clippy -D warnings`,
  `cargo test` (including the byte-compat gate for generated test templates), a
  no-CRLF guard (CRLF in a vendored base poisons the recorded diffs), and a check
  that the committed schemas match the code.
- `tutorials` runs every tutorial in the pinned container, weekly and on manual
  dispatch, and compares each manifest against the committed expected one
  A red step means the tutorial, or Tauri underneath it, broke.
- `image` publishes the container image to GHCR whenever the `Dockerfile`
  changes, tagged with the Dockerfile's content hash; `tutorials` pulls that
  tag and builds locally only when it has not been published yet.

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
