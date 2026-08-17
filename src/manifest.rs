use std::fmt::Write as _;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::snapshot::{self, Tree};
use crate::tutorial::{AssertionKind, Tutorial};

/// The committed contract between the runner and consumers (tauri-docs
/// components read this JSON — field names and order are load-bearing for the
/// diff-review workflow, so keep struct order stable).
/// v2: advisory/platform optional (stripped in blessed manifests), status
/// value `skipped` removed.
/// v3: mutations record file snapshots (`base/`, `steps/<step>/`) instead of
/// an embedded diff; consumers derive the diff from the pair.
pub const SCHEMA_VERSION: u32 = 3;

/// where the manifest points editors at its JSON Schema: one level above the
/// manifest, shared by the sibling tutorials in both repos
pub const SCHEMA_REF: &str = "../manifest.schema.json";

fn schema_ref() -> String {
  SCHEMA_REF.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
  /// relative path to the JSON Schema, so an editor validates hand edits
  #[serde(rename = "$schema", default = "schema_ref")]
  pub schema: String,
  /// bumped when the manifest shape changes; consumers assert on it
  #[serde(rename = "schemaVersion")]
  pub schema_version: u32,
  pub id: String,
  pub title: String,
  /// true when produced outside the pinned container (`tatu check`); absent,
  /// like platform, in the committed expected manifest (`tatu bless` strips
  /// the run-environment fields)
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub advisory: Option<bool>,
  /// node-compatible platform tag: win32 / darwin / linux
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub platform: Option<String>,
  pub steps: Vec<StepRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StepRecord {
  pub id: String,
  pub task: String,
  pub mutations: Vec<MutationRecord>,
  pub preconditions: Vec<ResultRecord>,
  pub assertions: Vec<ResultRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MutationRecord {
  /// the mutated file; its content after this step is `steps/<step>/<file>`
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file: Option<String>,
  /// true when the mutation created the file, so there is no `base/<file>`
  /// and consumers render the whole file instead of a diff
  #[serde(skip_serializing_if = "Option::is_none")]
  pub created: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub command: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultRecord {
  pub kind: AssertionKind,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub command: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub run: Option<String>,
  pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
  Pass,
  Fail,
}

// advisory and platform describe where a run happened, not what the tutorial
// does — the committed expected manifest carries only the portable fields.
// $schema is forced back to the ref so moving the schema shows up as a
// fixed-point failure rather than as silent drift.
fn strip_run_environment(mut manifest: Manifest) -> Manifest {
  manifest.schema = schema_ref();
  manifest.advisory = None;
  manifest.platform = None;
  manifest
}

pub(crate) fn render(manifest: &Manifest) -> Result<String> {
  Ok(serde_json::to_string_pretty(manifest)? + "\n")
}

/// the manifest JSON Schema as written next to a tutorial tree and under
/// `schemas/` — one rendering, so the two copies cannot drift
pub fn schema_json() -> Result<String> {
  Ok(serde_json::to_string_pretty(&schemars::schema_for!(Manifest))? + "\n")
}

pub fn normalized(json: &str) -> Result<String> {
  render(&strip_run_environment(serde_json::from_str(json)?))
}

/// The committed form of a tree: the manifest through the typed round-trip,
/// snapshots as read (already LF-normalized).
pub fn normalize_tree(tree: Tree) -> Result<Tree> {
  Ok(Tree {
    manifest: normalized(&tree.manifest)?,
    files: tree.files,
  })
}

/// The three ways a tree drifts: the manifest, which snapshots exist, and the
/// content of the shared ones. An empty report means identical.
pub fn compare_trees(expected: &Tree, fresh: &Tree) -> String {
  let mut report = String::new();
  if expected.manifest != fresh.manifest {
    report.push_str(&crate::helpers::unified_diff(
      &expected.manifest,
      &fresh.manifest,
      "expected/manifest.json",
      "fresh run",
    ));
  }
  for path in fresh.files.keys() {
    if !expected.files.contains_key(path) {
      writeln!(report, "  added     {path}").expect("write to string");
    }
  }
  for path in expected.files.keys() {
    if !fresh.files.contains_key(path) {
      writeln!(report, "  removed   {path}").expect("write to string");
    }
  }
  for (path, before) in &expected.files {
    let Some(after) = fresh.files.get(path) else {
      continue;
    };
    if before != after {
      report.push_str(&crate::helpers::unified_diff(
        before,
        after,
        &format!("expected/{path}"),
        &format!("fresh/{path}"),
      ));
    }
  }
  report
}

pub fn verify_expected(tutorial: &Tutorial) -> Result<()> {
  let fresh = normalized_tree(&tutorial.out_dir()?, "run the tutorial first")?;
  let expected = normalized_tree(
    &tutorial.expected_dir(),
    "`tatu bless` a reviewed run to create it",
  )?;
  let report = compare_trees(&expected, &fresh);
  if report.is_empty() {
    println!("manifest matches expected");
    return Ok(());
  }
  print!("{report}");
  Err(Error::Runner(
    "manifest drifted from expected — review the diff, then `tatu bless` to accept it".into(),
  ))
}

pub fn bless_expected(tutorial: &Tutorial) -> Result<()> {
  let fresh = read(&tutorial.out_dir()?, "run the tutorial first")?;
  let manifest: Manifest = serde_json::from_str(&fresh.manifest)?;
  let declared: Vec<&str> = tutorial.steps.iter().map(|s| s.id.as_str()).collect();
  check_blessable(&manifest, &declared)?;
  if manifest.advisory == Some(true) {
    println!("note: blessing an advisory (host) run — authoritative manifests come from `tatu run` in the container");
  }
  let expected = tutorial.expected_dir();
  check_bless_target(&expected)?;
  snapshot::write_tree(&expected, &strip_run_environment(manifest), &fresh.files)?;
  println!("wrote {}", expected.display());
  Ok(())
}

// a baseline must come from a complete, green run — blessing a failed or
// truncated manifest (a `--step` run, or a run that broke partway) would make
// `tatu verify` vouch for a broken tutorial from then on
fn check_blessable(manifest: &Manifest, declared_steps: &[&str]) -> Result<()> {
  let failed: Vec<&str> = manifest
    .steps
    .iter()
    .filter(|s| {
      s.preconditions
        .iter()
        .chain(s.assertions.iter())
        .any(|r| r.status == Status::Fail)
    })
    .map(|s| s.id.as_str())
    .collect();
  if !failed.is_empty() {
    return Err(Error::Runner(format!(
      "refusing to bless a failed run — failing step(s): {}",
      failed.join(", ")
    )));
  }
  let ran: Vec<&str> = manifest.steps.iter().map(|s| s.id.as_str()).collect();
  if ran != declared_steps {
    return Err(Error::Runner(format!(
      "refusing to bless an incomplete run — the manifest covers [{}] but tutorial.yaml declares [{}]; re-run without --step",
      ran.join(", "),
      declared_steps.join(", ")
    )));
  }
  Ok(())
}

// bless replaces the whole expected tree — whatever it replaces must itself
// be a tutorial tree
fn check_bless_target(target: &Path) -> Result<()> {
  if target.exists() && !target.join(snapshot::MANIFEST_FILE).exists() {
    return Err(Error::Runner(format!(
      "refusing to replace {} — it holds no {}, so it may not be a tutorial tree",
      target.display(),
      snapshot::MANIFEST_FILE
    )));
  }
  Ok(())
}

fn read(root: &Path, hint: &str) -> Result<Tree> {
  snapshot::read_tree(root).map_err(|e| Error::Runner(format!("{e} — {hint}")))
}

// a tree only means anything while its manifest and its snapshots agree, so
// the self-check runs before either side of a comparison is trusted
fn normalized_tree(root: &Path, hint: &str) -> Result<Tree> {
  let tree = read(root, hint)?;
  let manifest = strip_run_environment(serde_json::from_str(&tree.manifest)?);
  snapshot::check_tree_consistent(&manifest, &tree.files)?;
  Ok(Tree {
    manifest: render(&manifest)?,
    files: tree.files,
  })
}

pub fn platform() -> &'static str {
  if cfg!(windows) {
    "win32"
  } else if cfg!(target_os = "macos") {
    "darwin"
  } else {
    "linux"
  }
}

#[cfg(test)]
mod tests {
  use super::{check_blessable, normalized, Manifest};

  fn manifest(json: &str) -> Manifest {
    serde_json::from_str(json).unwrap()
  }

  const GREEN: &str = r#"{"schemaVersion":3,"id":"t","title":"T","steps":[
    {"id":"one","task":"","mutations":[],"preconditions":[],
     "assertions":[{"kind":"shell","status":"pass"}]},
    {"id":"two","task":"","mutations":[],"preconditions":[],
     "assertions":[{"kind":"shell","status":"pass"}]}]}"#;

  #[test]
  fn complete_green_run_is_blessable() {
    assert!(check_blessable(&manifest(GREEN), &["one", "two"]).is_ok());
  }

  #[test]
  fn failed_run_is_not_blessable() {
    let failed = GREEN.replacen("\"pass\"", "\"fail\"", 1);
    let err = check_blessable(&manifest(&failed), &["one", "two"]).unwrap_err();
    assert!(err.to_string().contains("failed run"), "{err}");
  }

  #[test]
  fn truncated_run_is_not_blessable() {
    let err = check_blessable(&manifest(GREEN), &["one", "two", "three"]).unwrap_err();
    assert!(err.to_string().contains("incomplete run"), "{err}");
  }

  #[test]
  fn normalized_strips_run_environment_fields() {
    let json =
      r#"{"schemaVersion":3,"id":"t","title":"T","advisory":true,"platform":"win32","steps":[]}"#;
    let n = normalized(json).unwrap();
    assert!(!n.contains("advisory"));
    assert!(!n.contains("platform"));
    assert!(n.contains("schemaVersion"));
  }

  // a manifest pointing somewhere else must not survive a bless unnoticed
  #[test]
  fn normalized_forces_the_schema_ref() {
    let json =
      r#"{"$schema":"./elsewhere.json","schemaVersion":3,"id":"t","title":"T","steps":[]}"#;
    assert!(normalized(json).unwrap().contains(super::SCHEMA_REF));
  }

  #[test]
  fn normalized_is_idempotent() {
    let json =
      r#"{"schemaVersion":3,"id":"t","title":"T","advisory":false,"platform":"linux","steps":[]}"#;
    let once = normalized(json).unwrap();
    assert_eq!(normalized(&once).unwrap(), once);
  }

  // the typed round-trip must reproduce what `tatu bless` committed — a field
  // reorder or serde attribute change here would make every tree "drift"
  #[test]
  fn committed_expected_trees_are_normalized_fixed_points() {
    let tutorials = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tutorials");
    let mut declared = 0;
    let mut seen = 0;
    for entry in std::fs::read_dir(tutorials).unwrap() {
      let dir = entry.unwrap().path();
      if !dir.join("tutorial.yaml").exists() {
        continue;
      }
      declared += 1;
      let tree = super::snapshot::read_tree(&dir.join("expected")).unwrap();
      assert_eq!(
        normalized(&tree.manifest).unwrap(),
        tree.manifest,
        "{}",
        dir.display()
      );
      super::snapshot::check_tree_consistent(&manifest(&tree.manifest), &tree.files).unwrap();
      seen += 1;
    }
    assert!(declared > 0, "no tutorials found");
    assert_eq!(seen, declared, "every tutorial carries a blessed tree");
  }
}
