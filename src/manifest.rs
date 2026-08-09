use std::fs;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::tutorial::Tutorial;

/// The committed contract between the runner and consumers (tauri-docs
/// components read this JSON — field names and order are load-bearing for the
/// diff-review workflow, so keep struct order stable).
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
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
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file: Option<String>,
  /// base-relative unified diff; empty string when the overlay changed nothing
  #[serde(skip_serializing_if = "Option::is_none")]
  pub diff: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub command: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultRecord {
  pub kind: String,
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
// does — the committed expected manifest carries only the portable fields
fn strip_run_environment(mut manifest: Manifest) -> Manifest {
  manifest.advisory = None;
  manifest.platform = None;
  manifest
}

fn render(manifest: &Manifest) -> Result<String> {
  Ok(serde_json::to_string_pretty(manifest)? + "\n")
}

pub fn normalized(json: &str) -> Result<String> {
  render(&strip_run_environment(serde_json::from_str(json)?))
}

pub fn verify_expected(tutorial: &Tutorial) -> Result<()> {
  let fresh = normalized(&read(
    tutorial.out_manifest_path()?,
    "run the tutorial first",
  )?)?;
  let expected = normalized(&read(
    tutorial.expected_manifest_path(),
    "`tatu bless` a reviewed run to create it",
  )?)?;
  if fresh == expected {
    println!("manifest matches expected");
    return Ok(());
  }
  print!(
    "{}",
    crate::helpers::unified_diff(&expected, &fresh, "expected.manifest.json", "fresh run")
  );
  Err(Error::Runner(
    "manifest drifted from expected — review the diff, then `tatu bless` to accept it".into(),
  ))
}

pub fn bless_expected(tutorial: &Tutorial) -> Result<()> {
  let raw = read(tutorial.out_manifest_path()?, "run the tutorial first")?;
  let manifest: Manifest = serde_json::from_str(&raw)?;
  if manifest.advisory == Some(true) {
    println!("note: blessing an advisory (host) run — authoritative manifests come from `tatu run` in the container");
  }
  let expected = tutorial.expected_manifest_path();
  fs::write(&expected, render(&strip_run_environment(manifest))?)?;
  println!("wrote {}", expected.display());
  Ok(())
}

fn read(path: std::path::PathBuf, hint: &str) -> Result<String> {
  fs::read_to_string(&path)
    .map_err(|e| Error::Runner(format!("cannot read {}: {e} — {hint}", path.display())))
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
  use super::normalized;

  #[test]
  fn normalized_strips_run_environment_fields() {
    let json =
      r#"{"schemaVersion":1,"id":"t","title":"T","advisory":true,"platform":"win32","steps":[]}"#;
    let n = normalized(json).unwrap();
    assert!(!n.contains("advisory"));
    assert!(!n.contains("platform"));
    assert!(n.contains("schemaVersion"));
  }

  #[test]
  fn normalized_is_idempotent() {
    let json =
      r#"{"schemaVersion":1,"id":"t","title":"T","advisory":false,"platform":"linux","steps":[]}"#;
    let once = normalized(json).unwrap();
    assert_eq!(normalized(&once).unwrap(), once);
  }

  // the typed round-trip must reproduce what `tatu bless` committed — a field
  // reorder or serde attribute change here would make every manifest "drift"
  #[test]
  fn committed_expected_manifests_are_normalized_fixed_points() {
    let tutorials = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tutorials");
    let mut seen = 0;
    for entry in std::fs::read_dir(tutorials).unwrap() {
      let path = entry.unwrap().path().join("expected.manifest.json");
      if !path.exists() {
        continue;
      }
      let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
      assert_eq!(normalized(&text).unwrap(), text, "{}", path.display());
      seen += 1;
    }
    assert!(seen >= 2, "expected manifests not found");
  }
}
