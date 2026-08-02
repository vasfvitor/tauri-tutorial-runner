# tauri-tutorial-runner

Executable, verifiable tutorials for [Tauri](https://tauri.app) apps. A tutorial is
declared as a base scaffold plus deterministic mutations plus programmatic
assertions; one YAML file compiles into a readable tutorial, a generatable template,
and a CI gate that fails when Tauri moves under the docs.

Status: v0, one pilot tutorial (`tutorials/greet-command`). Not published.

## Why

Tauri's docs CI never compiles the snippets embedded in its pages, so guides
drift as the framework moves: APIs change, scaffolds change, and readers are the
first to find out. A tutorial that runs is a tutorial that can't silently rot.

## Usage

```
pnpm install
node bin/tatu.js check tutorials/greet-command    # advisory run on your machine
node bin/tatu.js validate tutorials/greet-command # parse + validate only
```

`tatu check` restores the tutorial's vendored base scaffold into `.tatu/work/`,
applies each step's mutations, generates one Rust integration test per assertion
phase, and runs them with `cargo test`. Host runs are advisory: results depend on
your platform's toolchain and webview stack. Authoritative runs (`tatu run`) are
reserved for the pinned container and are what will write the committed
`tutorial.manifest.json`.

The `ipc-acl` assertion kind tests real capability ACL behavior headlessly: the
generated tests build the app with `tauri::generate_context!()`, which resolves the
actual `capabilities/*.json` under `cargo test` without any frontend build. A
tutorial step can assert that a plugin command is denied before a permission is
granted and succeeds after — the silent-ACL-failure class, made loud.

## Tutorial anatomy

```
tutorials/<id>/
  tutorial.yaml   # steps: task (agent prompt) + mutations + assertions
  base/           # vendored scaffold (create-tauri-app output, committed)
  steps/<step>/   # overlay files, copied over the work tree by that step
```

Overlays are the authoring surface; the runner derives base-relative diffs from
them as the canonical record (a raw overlay would silently revert scaffold changes
it doesn't discuss; a diff fails loudly).

## Windows note

`cargo test` executables that link tauri with default features crash at load with
`STATUS_ENTRYPOINT_NOT_FOUND`, because the comctl32 v6 manifest that Tauri's build
script embeds into app binaries never reaches test executables. The runner injects
the manifest into the work tree automatically (`rustc-link-arg-tests`), which is
also why generated tests are integration-test targets: link args never reach
`#[cfg(test)]` modules inside `src/`.
