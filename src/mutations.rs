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
  let after = to_json_matching_style(&merged, &before)?;
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

// the recorded diff must show the merge, not a reflow — match the target
// file's own indentation and trailing-newline style
fn to_json_matching_style(value: &serde_json::Value, original: &str) -> Result<String> {
  use serde::Serialize;
  let indent = detect_indent(original);
  let mut buf = Vec::new();
  let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
  let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
  value.serialize(&mut serializer)?;
  let mut out = String::from_utf8(buf).expect("serde_json emits utf8");
  if original.ends_with('\n') {
    out.push('\n');
  }
  Ok(out)
}

fn detect_indent(text: &str) -> String {
  text
    .lines()
    .find_map(|line| {
      let ws: String = line.chars().take_while(|c| c.is_whitespace()).collect();
      (!ws.is_empty() && ws.len() < line.len()).then_some(ws)
    })
    .unwrap_or_else(|| "  ".to_string())
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
  use super::{overlay_reverted_lines, to_json_matching_style};

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

  #[test]
  fn json_style_keeps_four_space_indent_and_missing_newline() {
    let original = "{\n    \"a\": 1,\n    \"b\": {\n        \"c\": 2\n    }\n}";
    let value: serde_json::Value = serde_json::from_str(original).unwrap();
    assert_eq!(to_json_matching_style(&value, original).unwrap(), original);
  }

  #[test]
  fn json_style_keeps_tabs_and_trailing_newline() {
    let original = "{\n\t\"a\": 1\n}\n";
    let value: serde_json::Value = serde_json::from_str(original).unwrap();
    assert_eq!(to_json_matching_style(&value, original).unwrap(), original);
  }

  #[test]
  fn json_style_defaults_to_two_spaces() {
    let value: serde_json::Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
    assert_eq!(
      to_json_matching_style(&value, "{}\n").unwrap(),
      "{\n  \"a\": 1\n}\n"
    );
  }
}
