use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tutorial {
  pub id: String,
  pub title: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub meta: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub env: Option<serde_json::Value>,
  pub base: Base,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub harness: Option<Harness>,
  pub steps: Vec<Step>,
  #[serde(skip)]
  #[schemars(skip)]
  pub dir: PathBuf,
  #[serde(skip)]
  #[schemars(skip)]
  pub fixture_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Base {
  /// vendored scaffold dir, relative to the tutorial dir (v0: required)
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub fixture: Option<String>,
  /// re-scaffolding commands — parsed but not implemented in v0
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub setup: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Harness {
  /// verbatim Rust included in generated tests (e.g. a copy of an app command)
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub prelude: Option<String>,
  /// names passed to tauri::generate_handler![]
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub handlers: Option<Vec<String>>,
  /// plugin crates registered on the mock builder (`<name>::init()`)
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub plugins: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
  pub id: String,
  /// natural-language instruction — doubles as the agent-eval prompt
  pub task: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub harness: Option<Harness>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub preconditions: Vec<Assertion>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub mutations: Vec<Mutation>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub assertions: Vec<Assertion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "engine", rename_all = "kebab-case")]
pub enum Mutation {
  /// copy steps/<step-id>/** over the work tree
  Overlay {},
  /// deep-merge into a JSON file (objects recurse, arrays union)
  JsonMerge {
    file: String,
    merge: serde_json::Value,
  },
  /// run a command in the work tree
  Shell {
    run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
  },
}

// only kinds the runner actually executes belong here — a kind that parses but
// skips would let a tutorial read as verified without anything having run.
// Future kinds (frontend build, e2e) get added when they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AssertionKind {
  IpcLocal,
  IpcAcl,
  Shell,
}

impl AssertionKind {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::IpcLocal => "ipc-local",
      Self::IpcAcl => "ipc-acl",
      Self::Shell => "shell",
    }
  }

  pub fn is_ipc(&self) -> bool {
    matches!(self, Self::IpcLocal | Self::IpcAcl)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
  pub kind: AssertionKind,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub command: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub args: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expect: Option<Expect>,
  /// verbatim Rust inserted before the invoke in the generated test
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub setup: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Expect {
  Keyword(ExpectKeyword),
  Ok { ok: serde_json::Value },
  OkContains { ok_contains: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExpectKeyword {
  Denied,
}

impl Tutorial {
  /// two levels above the tutorial dir (`tutorials/<id>`)
  pub fn repo_root(&self) -> Result<PathBuf> {
    self
      .dir
      .parent()
      .and_then(Path::parent)
      .map(Path::to_path_buf)
      .ok_or_else(|| Error::Runner("tutorial dir has no repo root two levels up".into()))
  }

  /// where a run writes its manifest
  pub fn out_manifest_path(&self) -> Result<PathBuf> {
    Ok(
      self
        .repo_root()?
        .join(".tatu")
        .join("out")
        .join(&self.id)
        .join("tutorial.manifest.json"),
    )
  }

  /// the committed manifest CI compares runs against
  pub fn expected_manifest_path(&self) -> PathBuf {
    self.dir.join("expected.manifest.json")
  }
}

pub fn load_tutorial(dir: &Path) -> Result<Tutorial> {
  let yaml_path = dir.join("tutorial.yaml");
  if !yaml_path.exists() {
    return Err(Error::Validate(format!(
      "no tutorial.yaml in {}",
      dir.display()
    )));
  }
  let mut tutorial: Tutorial = serde_norway::from_str(&fs::read_to_string(&yaml_path)?)?;
  tutorial.dir = dir.to_path_buf();

  let Some(fixture) = &tutorial.base.fixture else {
    return Err(Error::Validate(
      "tutorial.yaml: v0 requires base.fixture (a vendored scaffold dir); base.setup is not implemented yet".into(),
    ));
  };
  tutorial.fixture_dir = dir.join(fixture);
  if !tutorial.fixture_dir.exists() {
    return Err(Error::Validate(format!(
      "base.fixture dir not found: {}",
      tutorial.fixture_dir.display()
    )));
  }

  let mut seen = HashSet::new();
  for step in &tutorial.steps {
    if !seen.insert(step.id.clone()) {
      return Err(Error::Validate(format!(
        "tutorial.yaml: duplicate step id \"{}\"",
        step.id
      )));
    }
    if step.task.trim().is_empty() {
      return Err(Error::Validate(format!(
        "step \"{}\": missing task (the agent-eval prompt)",
        step.id
      )));
    }

    for mutation in &step.mutations {
      if matches!(mutation, Mutation::Overlay {}) {
        let overlay_dir = dir.join("steps").join(&step.id);
        if !overlay_dir.exists() {
          return Err(Error::Validate(format!(
            "step \"{}\": overlay mutation but no steps/{}/ dir",
            step.id, step.id
          )));
        }
      }
    }

    for assertion in step.preconditions.iter().chain(step.assertions.iter()) {
      // parse-time guard: mock-context-shaped assertions can
      // never pass for plugin commands, so reject the combination outright
      if assertion.kind == AssertionKind::IpcLocal
        && assertion
          .command
          .as_deref()
          .unwrap_or_default()
          .starts_with("plugin:")
      {
        return Err(Error::Validate(format!(
          "step \"{}\": ipc-local cannot target \"{}\" — plugin commands need ipc-acl (resolved capability ACL)",
          step.id,
          assertion.command.as_deref().unwrap_or_default()
        )));
      }
      if assertion.kind.is_ipc() {
        if assertion.command.is_none() {
          return Err(Error::Validate(format!(
            "step \"{}\": {} needs command",
            step.id,
            assertion.kind.as_str()
          )));
        }
        if assertion.expect.is_none() {
          return Err(Error::Validate(format!(
            "step \"{}\": {} needs expect (ok / ok_contains / denied)",
            step.id,
            assertion.kind.as_str()
          )));
        }
      }
      if assertion.kind == AssertionKind::Shell && assertion.run.is_none() {
        return Err(Error::Validate(format!(
          "step \"{}\": shell assertion needs run",
          step.id
        )));
      }
    }
  }

  Ok(tutorial)
}
