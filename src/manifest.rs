use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
  /// true when produced outside the pinned container (`tatu check`)
  pub advisory: bool,
  /// node-compatible platform tag: win32 / darwin / linux
  pub platform: String,
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
  Skipped,
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
