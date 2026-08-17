use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::helpers;
use crate::manifest::Manifest;

pub const MANIFEST_FILE: &str = "manifest.json";
pub const SCHEMA_FILE: &str = "manifest.schema.json";

/// What a run observed on disk: `base` holds a file's content before the
/// tutorial first touched it, `steps` its content after each step that mutates
/// it. Consumers derive the diffs readers see from that pair, which is why the
/// pair — not a rendered diff — is what gets recorded.
#[derive(Debug, Default)]
pub struct Snapshots {
  base: BTreeMap<String, Capture>,
  steps: BTreeMap<(String, String), String>,
  /// file → its most recent capture; the before a consumer derives
  latest: BTreeMap<String, Capture>,
}

#[derive(Debug)]
struct Capture {
  step: String,
  content: String,
}

impl Snapshots {
  /// Record one file mutation from what the applier saw on disk, returning
  /// whether the mutation created the file.
  ///
  /// The derivation invariant lives here: a consumer takes the file's before
  /// from the previous snapshot, so that snapshot must be what this step
  /// actually started from. Anything that rewrote the file between the two —
  /// a shell mutation, an assertion command, the harness guard — breaks the
  /// record, and a broken record is only visible to readers, never to CI.
  pub fn record_file(
    &mut self,
    step_id: &str,
    path: &str,
    before: Option<&str>,
    after: &str,
  ) -> Result<bool> {
    validate_rel_path(path)?;
    let before = before.map(normalize_lf);
    let after = normalize_lf(after);
    let key = (step_id.to_string(), path.to_string());
    if self.steps.contains_key(&key) {
      return Err(Error::Runner(format!(
        "step \"{step_id}\" records {path} twice — a step keeps one snapshot per file; merge the mutations or split the step"
      )));
    }

    let created = match self.latest.get(path) {
      Some(previous) => {
        let Some(observed) = before.as_deref() else {
          return Err(Error::Runner(format!(
            "{path} was deleted between step \"{}\" and step \"{step_id}\" — snapshots record content, not deletions; restructure the tutorial so the file survives",
            previous.step
          )));
        };
        if observed != previous.content {
          return Err(Error::Runner(format!(
            "{path} changed between step \"{}\" and step \"{step_id}\" outside a recorded mutation:\n{}\
             something between the steps — a shell mutation, or an assertion command such as `pnpm install` or `pnpm build` — rewrote the file. Consumers derive this step's diff from the two snapshots, so they must agree: move the offending command into the step that mutates the file, or record the file where it actually changes.",
            previous.step,
            helpers::unified_diff(
              &previous.content,
              observed,
              &format!("recorded steps/{}/{path}", previous.step),
              &format!("observed before step {step_id}"),
            ),
          )));
        }
        false
      }
      None => match before.as_deref() {
        Some(observed) => {
          self.base.insert(
            path.to_string(),
            Capture {
              step: step_id.to_string(),
              content: observed.to_string(),
            },
          );
          false
        }
        None => true,
      },
    };

    self.steps.insert(key, after.clone());
    self.latest.insert(
      path.to_string(),
      Capture {
        step: step_id.to_string(),
        content: after,
      },
    );
    Ok(created)
  }

  /// the tree-relative files a run writes: `base/<file>` and
  /// `steps/<step>/<file>`
  pub fn tree_files(&self) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for (file, capture) in &self.base {
      files.insert(format!("base/{file}"), capture.content.clone());
    }
    for ((step, file), content) in &self.steps {
      files.insert(format!("steps/{step}/{file}"), content.clone());
    }
    files
  }

  /// every snapshot first written while running `step_id` — its own step
  /// snapshots plus any base captured there
  pub fn recorded_in(&self, step_id: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    for (file, capture) in &self.base {
      if capture.step == step_id {
        out.push((format!("base/{file}"), capture.content.as_str()));
      }
    }
    for ((step, file), content) in &self.steps {
      if step == step_id {
        out.push((format!("steps/{step}/{file}"), content.as_str()));
      }
    }
    out
  }
}

/// A tutorial tree read back from disk: the manifest verbatim plus every
/// snapshot, keyed by tree-relative path.
#[derive(Debug, Clone)]
pub struct Tree {
  pub manifest: String,
  pub files: BTreeMap<String, String>,
}

pub fn read_tree(root: &Path) -> Result<Tree> {
  if !root.is_dir() {
    return Err(Error::Runner(format!(
      "no tutorial tree at {}",
      root.display()
    )));
  }
  let mut manifest = None;
  let mut files = BTreeMap::new();
  for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
    let entry = entry.map_err(|e| Error::Runner(e.to_string()))?;
    if !entry.file_type().is_file() {
      continue;
    }
    let rel = helpers::rel_slash(root, entry.path());
    let text = normalize_lf(&fs::read_to_string(entry.path())?);
    if rel == MANIFEST_FILE {
      manifest = Some(text);
    } else if rel.starts_with("base/") || rel.starts_with("steps/") {
      files.insert(rel, text);
    } else {
      return Err(Error::Runner(format!(
        "unexpected entry {rel} in {} — a tutorial tree holds {MANIFEST_FILE}, base/** and steps/**",
        root.display()
      )));
    }
  }
  let manifest =
    manifest.ok_or_else(|| Error::Runner(format!("no {MANIFEST_FILE} in {}", root.display())))?;
  Ok(Tree { manifest, files })
}

/// Write a tutorial tree: the manifest, its snapshots, and the JSON Schema the
/// manifest's `$schema` points at (one level up, shared by sibling tutorials).
/// The root is replaced wholesale — a snapshot from an older run must never
/// survive into the tree that replaces it.
pub fn write_tree(
  root: &Path,
  manifest: &Manifest,
  files: &BTreeMap<String, String>,
) -> Result<()> {
  check_tree_consistent(manifest, files)?;
  let parent = root.parent().ok_or_else(|| {
    Error::Runner(format!(
      "refusing to write a tutorial tree at {} — it has no parent directory",
      root.display()
    ))
  })?;
  if root.exists() {
    fs::remove_dir_all(root)?;
  }
  fs::create_dir_all(root)?;
  fs::write(root.join(MANIFEST_FILE), crate::manifest::render(manifest)?)?;
  for (rel, content) in files {
    let dest = root.join(rel);
    if let Some(dir) = dest.parent() {
      fs::create_dir_all(dir)?;
    }
    fs::write(dest, content)?;
  }
  fs::write(parent.join(SCHEMA_FILE), crate::manifest::schema_json()?)?;
  Ok(())
}

/// The convention that links a manifest to its snapshots: every file mutation
/// has `steps/<step>/<file>`; a file's first mutation has `base/<file>` unless
/// it created the file; a shell mutation names no file; nothing else lives in
/// the tree. Both repos check this — manifest and snapshots are a contract
/// only while they agree.
pub fn check_tree_consistent(manifest: &Manifest, files: &BTreeMap<String, String>) -> Result<()> {
  let mut expected: BTreeSet<String> = BTreeSet::new();
  let mut seen: BTreeSet<&str> = BTreeSet::new();
  for step in &manifest.steps {
    let step_id = &step.id;
    for mutation in &step.mutations {
      match (&mutation.file, mutation.created) {
        (Some(file), Some(created)) => {
          validate_rel_path(file)?;
          if mutation.command.is_some() || mutation.cwd.is_some() {
            return Err(inconsistent(format!(
              "step \"{step_id}\" records {file} with a command — a file mutation carries no command"
            )));
          }
          let first = seen.insert(file.as_str());
          if created && !first {
            return Err(inconsistent(format!(
              "step \"{step_id}\" marks {file} created, but an earlier step already recorded it"
            )));
          }
          expected.insert(format!("steps/{step_id}/{file}"));
          if first && !created {
            expected.insert(format!("base/{file}"));
          }
        }
        (Some(file), None) => {
          return Err(inconsistent(format!(
            "step \"{step_id}\" records {file} without `created`"
          )))
        }
        (None, Some(_)) => {
          return Err(inconsistent(format!(
            "step \"{step_id}\" has a `created` mutation that names no file"
          )))
        }
        (None, None) if mutation.command.is_none() => {
          return Err(inconsistent(format!(
            "step \"{step_id}\" has a mutation that names neither a file nor a command"
          )))
        }
        (None, None) => {}
      }
    }
  }
  for path in &expected {
    if !files.contains_key(path) {
      return Err(inconsistent(format!(
        "{path} is missing — the manifest records that mutation"
      )));
    }
  }
  for path in files.keys() {
    if !expected.contains(path) {
      return Err(inconsistent(format!(
        "{path} is orphaned — no mutation in the manifest records it"
      )));
    }
  }
  Ok(())
}

fn inconsistent(detail: String) -> Error {
  Error::Runner(format!("tree inconsistent: {detail}"))
}

// snapshot paths become real paths under base/ and steps/<step>/ — one that
// escapes the tree (or names a drive) would write outside it. `file:` in a
// json-merge mutation comes from hand-written YAML, so this is a real check,
// not an assertion about the runner's own output.
fn validate_rel_path(path: &str) -> Result<()> {
  let fault = if path.is_empty() {
    "is empty"
  } else if path.starts_with('/') {
    "is absolute"
  } else if path.contains('\\') {
    "uses backslashes — snapshot paths are slash-separated"
  } else if path.chars().nth(1) == Some(':') {
    "names a drive"
  } else if path.split('/').any(|part| part == "..") {
    "escapes the tree with .."
  } else if path.split('/').any(str::is_empty) {
    "has an empty path component"
  } else {
    return Ok(());
  };
  Err(Error::Runner(format!("mutation file \"{path}\" {fault}")))
}

fn normalize_lf(text: &str) -> String {
  text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
  use super::{check_tree_consistent, Snapshots};
  use crate::manifest::Manifest;
  use std::collections::BTreeMap;

  fn manifest(steps: &str) -> Manifest {
    serde_json::from_str(&format!(
      r#"{{"schemaVersion":3,"id":"t","title":"T","steps":[{steps}]}}"#
    ))
    .unwrap()
  }

  fn step(id: &str, mutations: &str) -> String {
    format!(
      r#"{{"id":"{id}","task":"","mutations":[{mutations}],"preconditions":[],"assertions":[]}}"#
    )
  }

  fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect()
  }

  #[test]
  fn first_record_captures_the_base() {
    let mut snaps = Snapshots::default();
    let created = snaps
      .record_file("one", "src/lib.rs", Some("old\n"), "new\n")
      .unwrap();
    assert!(!created);
    assert_eq!(
      snaps.tree_files(),
      files(&[
        ("base/src/lib.rs", "old\n"),
        ("steps/one/src/lib.rs", "new\n")
      ])
    );
  }

  #[test]
  fn a_created_file_has_no_base() {
    let mut snaps = Snapshots::default();
    let created = snaps
      .record_file("one", "rsbuild.config.ts", None, "x\n")
      .unwrap();
    assert!(created);
    assert_eq!(
      snaps.tree_files(),
      files(&[("steps/one/rsbuild.config.ts", "x\n")])
    );
  }

  #[test]
  fn a_later_step_derives_its_before_from_the_earlier_snapshot() {
    let mut snaps = Snapshots::default();
    snaps
      .record_file("one", "src/lib.rs", Some("old\n"), "mid\n")
      .unwrap();
    let created = snaps
      .record_file("two", "src/lib.rs", Some("mid\n"), "new\n")
      .unwrap();
    assert!(!created);
    assert_eq!(
      snaps.tree_files(),
      files(&[
        ("base/src/lib.rs", "old\n"),
        ("steps/one/src/lib.rs", "mid\n"),
        ("steps/two/src/lib.rs", "new\n"),
      ])
    );
  }

  // the invariant that makes a derived diff trustworthy — the error has to
  // name both steps and show what moved, or the author cannot act on it
  #[test]
  fn a_change_between_steps_is_an_actionable_error() {
    let mut snaps = Snapshots::default();
    snaps
      .record_file("one", "package.json", Some("{}\n"), "{\"a\":1}\n")
      .unwrap();
    let err = snaps
      .record_file("two", "package.json", Some("{\"a\":2}\n"), "{\"a\":3}\n")
      .unwrap_err()
      .to_string();
    assert!(
      err.contains("changed between step \"one\" and step \"two\""),
      "{err}"
    );
    assert!(err.contains("-{\"a\":1}"), "{err}");
    assert!(err.contains("+{\"a\":2}"), "{err}");
    assert!(err.contains("pnpm install"), "{err}");
  }

  #[test]
  fn a_file_deleted_between_steps_is_an_error() {
    let mut snaps = Snapshots::default();
    snaps
      .record_file("one", "src/lib.rs", Some("old\n"), "new\n")
      .unwrap();
    let err = snaps
      .record_file("two", "src/lib.rs", None, "fresh\n")
      .unwrap_err()
      .to_string();
    assert!(err.contains("was deleted between"), "{err}");
  }

  #[test]
  fn the_same_file_twice_in_a_step_is_an_error() {
    let mut snaps = Snapshots::default();
    snaps
      .record_file("one", "src/lib.rs", Some("old\n"), "new\n")
      .unwrap();
    let err = snaps
      .record_file("one", "src/lib.rs", Some("new\n"), "newer\n")
      .unwrap_err()
      .to_string();
    assert!(err.contains("twice"), "{err}");
  }

  // CRLF work trees are the Windows default; normalizing at record time keeps
  // the invariant comparing content, not line endings
  #[test]
  fn crlf_is_normalized_before_the_compare() {
    let mut snaps = Snapshots::default();
    snaps
      .record_file("one", "src/lib.rs", Some("old\r\n"), "new\r\nline\r\n")
      .unwrap();
    snaps
      .record_file("two", "src/lib.rs", Some("new\nline\n"), "final\n")
      .unwrap();
    assert_eq!(snaps.tree_files()["steps/one/src/lib.rs"], "new\nline\n");
  }

  #[test]
  fn paths_that_escape_the_tree_are_rejected() {
    let mut snaps = Snapshots::default();
    for path in ["", "/etc/passwd", "../outside", "C:/win", "a\\b"] {
      assert!(
        snaps.record_file("one", path, Some("a"), "b").is_err(),
        "accepted {path}"
      );
    }
  }

  #[test]
  fn a_consistent_tree_passes() {
    let m = manifest(&format!(
      "{},{}",
      step(
        "one",
        r#"{"file":"src/lib.rs","created":false},{"command":"pnpm add x","cwd":"."}"#
      ),
      step("two", r#"{"file":"new.ts","created":true}"#)
    ));
    check_tree_consistent(
      &m,
      &files(&[
        ("base/src/lib.rs", ""),
        ("steps/one/src/lib.rs", ""),
        ("steps/two/new.ts", ""),
      ]),
    )
    .unwrap();
  }

  #[test]
  fn a_missing_snapshot_is_caught() {
    let m = manifest(&step("one", r#"{"file":"src/lib.rs","created":false}"#));
    let err = check_tree_consistent(&m, &files(&[("steps/one/src/lib.rs", "")]))
      .unwrap_err()
      .to_string();
    assert!(err.contains("base/src/lib.rs is missing"), "{err}");
  }

  #[test]
  fn an_orphan_snapshot_is_caught() {
    let m = manifest(&step("one", r#"{"file":"src/lib.rs","created":true}"#));
    let err = check_tree_consistent(
      &m,
      &files(&[("steps/one/src/lib.rs", ""), ("steps/one/stray.ts", "")]),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("steps/one/stray.ts is orphaned"), "{err}");
  }

  #[test]
  fn a_created_file_must_not_carry_a_base() {
    let m = manifest(&step("one", r#"{"file":"new.ts","created":true}"#));
    let err = check_tree_consistent(&m, &files(&[("steps/one/new.ts", ""), ("base/new.ts", "")]))
      .unwrap_err()
      .to_string();
    assert!(err.contains("base/new.ts is orphaned"), "{err}");
  }
}
