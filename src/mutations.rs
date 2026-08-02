use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::manifest::MutationRecord;
use crate::tutorial::Tutorial;

// Overlays are the authoring surface; the derived base-relative diff is the
// canonical record.
pub fn apply_overlay(
  tutorial: &Tutorial,
  step_id: &str,
  work_dir: &Path,
) -> Result<Vec<MutationRecord>> {
  let overlay_dir = tutorial.dir.join("steps").join(step_id);
  let mut applied = Vec::new();
  for entry in walkdir::WalkDir::new(&overlay_dir).sort_by_file_name() {
    let entry = entry.map_err(|e| Error::Runner(e.to_string()))?;
    if !entry.file_type().is_file() {
      continue;
    }
    let rel = entry
      .path()
      .strip_prefix(&overlay_dir)
      .expect("walkdir yields children of its root")
      .to_string_lossy()
      .replace('\\', "/");
    let dest = work_dir.join(&rel);
    let before = dest
      .exists()
      .then(|| fs::read_to_string(&dest))
      .transpose()?;
    if let Some(parent) = dest.parent() {
      fs::create_dir_all(parent)?;
    }
    fs::copy(entry.path(), &dest)?;
    let after = fs::read_to_string(&dest)?;
    applied.push(MutationRecord {
      file: Some(rel.clone()),
      diff: Some(unified_diff(before.as_deref(), &after, &rel)),
      command: None,
      cwd: None,
    });
  }
  Ok(applied)
}

pub fn apply_json_merge(
  file: &str,
  merge: &serde_json::Value,
  work_dir: &Path,
) -> Result<Vec<MutationRecord>> {
  let target = work_dir.join(file);
  let before = fs::read_to_string(&target)?;
  let merged = deep_merge(serde_json::from_str(&before)?, merge);
  let after = serde_json::to_string_pretty(&merged)? + "\n";
  fs::write(&target, &after)?;
  Ok(vec![MutationRecord {
    file: Some(file.to_string()),
    diff: Some(unified_diff(Some(&before), &after, file)),
    command: None,
    cwd: None,
  }])
}

pub fn apply_shell(run: &str, cwd: Option<&str>, work_dir: &Path) -> Result<Vec<MutationRecord>> {
  let dir = match cwd {
    Some(sub) => work_dir.join(sub),
    None => work_dir.to_path_buf(),
  };
  let output = shell_command(run).current_dir(&dir).output()?;
  if !output.status.success() {
    return Err(Error::Runner(format!(
      "shell mutation failed ({run}):\n{}\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    )));
  }
  Ok(vec![MutationRecord {
    file: None,
    diff: None,
    command: Some(run.to_string()),
    cwd: Some(cwd.unwrap_or(".").to_string()),
  }])
}

pub fn shell_command(run: &str) -> Command {
  #[cfg(windows)]
  {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", run]);
    cmd
  }
  #[cfg(not(windows))]
  {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", run]);
    cmd
  }
}

// objects merge recursively; arrays are a union (append missing entries) — the
// capabilities-permissions case this exists for
fn deep_merge(base: serde_json::Value, patch: &serde_json::Value) -> serde_json::Value {
  use serde_json::Value;
  match (base, patch) {
    (Value::Array(base_items), Value::Array(patch_items)) => {
      let mut out = base_items;
      let seen: Vec<String> = out.iter().map(|v| v.to_string()).collect();
      for item in patch_items {
        if !seen.contains(&item.to_string()) {
          out.push(item.clone());
        }
      }
      Value::Array(out)
    }
    (Value::Object(mut base_map), Value::Object(patch_map)) => {
      for (key, value) in patch_map {
        let merged = match base_map.get(key) {
          Some(existing) => deep_merge(existing.clone(), value),
          None => value.clone(),
        };
        base_map.insert(key.clone(), merged);
      }
      Value::Object(base_map)
    }
    (_, patch) => patch.clone(),
  }
}

fn unified_diff(before: Option<&str>, after: &str, label: &str) -> String {
  match before {
    None => format!(
      "new file: {label}\n+{}",
      after.split('\n').collect::<Vec<_>>().join("\n+")
    ),
    Some(before) if before == after => String::new(),
    Some(before) => {
      let diff = similar::TextDiff::from_lines(before, after);
      diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{label}"), &format!("b/{label}"))
        .to_string()
    }
  }
}
