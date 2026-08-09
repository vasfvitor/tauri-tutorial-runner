use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::helpers;
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
    let rel = helpers::rel_slash(&overlay_dir, entry.path());
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
  let after = crate::json_style::render_merged(&before, &merged)?;
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
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd");
    // raw_arg: std's default quoting escapes inner quotes as \" which cmd
    // does not parse — the command line must reach cmd verbatim
    cmd.arg("/C").raw_arg(run);
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

// Re-vendor guard: an overlay wholesale-replaces its base
// file, so base lines the recorded tutorial diff never removed must survive.
// Returns the base lines the fresh diff removes beyond the recorded one —
// non-empty means the base changed under the overlay and applying it would
// silently revert scaffold content the tutorial never discusses.
pub fn overlay_reverted_lines(recorded: &str, fresh: &str) -> Vec<String> {
  fn removed_counts(diff: &str) -> std::collections::HashMap<&str, u32> {
    let mut counts = std::collections::HashMap::new();
    for line in diff.lines() {
      if line.starts_with("---") {
        continue;
      }
      if let Some(rest) = line.strip_prefix('-') {
        *counts.entry(rest).or_default() += 1;
      }
    }
    counts
  }
  let recorded = removed_counts(recorded);
  let mut offending: Vec<String> = removed_counts(fresh)
    .into_iter()
    .filter(|(line, n)| *n > recorded.get(line).copied().unwrap_or(0))
    .map(|(line, _)| line.to_string())
    .collect();
  offending.sort();
  offending
}

fn unified_diff(before: Option<&str>, after: &str, label: &str) -> String {
  match before {
    None => format!(
      "new file: {label}\n+{}",
      after.split('\n').collect::<Vec<_>>().join("\n+")
    ),
    Some(before) if before == after => String::new(),
    Some(before) => {
      helpers::unified_diff(before, after, &format!("a/{label}"), &format!("b/{label}"))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{deep_merge, overlay_reverted_lines, unified_diff};

  const RECORDED: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n context\n-old_line\n+new_line\n+added_line\n context2\n";

  #[test]
  fn identical_diff_passes() {
    assert!(overlay_reverted_lines(RECORDED, RECORDED).is_empty());
  }

  #[test]
  fn extra_removal_is_flagged() {
    let fresh = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,4 @@\n context\n-old_line\n-upstream_new_line\n+new_line\n context2\n";
    assert_eq!(
      overlay_reverted_lines(RECORDED, fresh),
      vec!["upstream_new_line".to_string()]
    );
  }

  #[test]
  fn changed_additions_alone_pass() {
    // tutorial author edited what the overlay adds — not a revert
    let fresh = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n context\n-old_line\n+different_new_line\n context2\n";
    assert!(overlay_reverted_lines(RECORDED, fresh).is_empty());
  }

  #[test]
  fn header_minus_lines_ignored() {
    assert!(overlay_reverted_lines("", "--- a/x\n+++ b/x\n").is_empty());
  }

  // the exact greet-tutorial merge against the real pool base — its diff is
  // committed in expected.manifest.json, so a rendering change here means
  // that manifest must be re-blessed
  #[test]
  fn greet_capability_merge_diff_is_minimal() {
    let before = std::fs::read_to_string(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bases/vanilla-ts@4.7.3/src-tauri/capabilities/default.json"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    let merge = serde_json::json!({
      "permissions": ["fs:default", "fs:allow-appdata-read-recursive"]
    });
    let merged = deep_merge(serde_json::from_str(&before).unwrap(), &merge);
    let after = crate::json_style::render_merged(&before, &merged).unwrap();
    let diff = unified_diff(Some(&before), &after, "src-tauri/capabilities/default.json");
    assert_eq!(
      diff,
      "--- a/src-tauri/capabilities/default.json\n\
       +++ b/src-tauri/capabilities/default.json\n\
       @@ -5,6 +5,8 @@\n   \"windows\": [\"main\"],\n   \"permissions\": [\n     \"core:default\",\n\
       -    \"opener:default\"\n+    \"opener:default\",\n+    \"fs:default\",\n\
       +    \"fs:allow-appdata-read-recursive\"\n   ]\n }\n"
    );
  }
}
