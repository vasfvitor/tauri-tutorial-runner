use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::helpers;
use crate::tutorial::Tutorial;

/// What an applier did. File mutations hand back the content they saw on
/// disk — the runner records that pair as snapshots, and consumers derive the
/// diff from it. Shell mutations hand back the same pair for every text file
/// the command changed, minus lockfiles and build output; the runner records
/// those as ordinary file snapshots after the command record, so `cargo add`
/// steps render as diffs instead of a bare command string.
#[derive(Debug)]
pub enum Applied {
  File {
    path: String,
    before: Option<String>,
    after: String,
  },
  Shell {
    command: String,
    cwd: String,
    files: Vec<FileChange>,
  },
}

/// One file a shell mutation changed, in the same shape a file mutation
/// records: `before` is `None` when the command created the file.
#[derive(Debug)]
pub struct FileChange {
  pub path: String,
  pub before: Option<String>,
  pub after: String,
}

// Overlays are the authoring surface; the file content on either side of the
// copy is the canonical record.
pub fn apply_overlay(tutorial: &Tutorial, step_id: &str, work_dir: &Path) -> Result<Vec<Applied>> {
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
    applied.push(Applied::File {
      path: rel,
      before,
      after,
    });
  }
  Ok(applied)
}

pub fn apply_json_merge(
  file: &str,
  merge: &serde_json::Value,
  work_dir: &Path,
) -> Result<Vec<Applied>> {
  let target = work_dir.join(file);
  let before = fs::read_to_string(&target)?;
  let merged = deep_merge(serde_json::from_str(&before)?, merge);
  let after = crate::json_style::render_merged(&before, &merged)?;
  fs::write(&target, &after)?;
  Ok(vec![Applied::File {
    path: file.to_string(),
    before: Some(before),
    after,
  }])
}

/// Directories a shell snapshot never descends into, matched by bare name at
/// any depth. Dependency stores and build output (`node_modules`, `target`,
/// `dist`, `.pnpm-store`, `gen`) are not tutorial content, and `.tatu` and
/// `tests` are the harness's own scratch. `gen` and `tests` are safe to match
/// by bare name because the only ones a scaffold has live under `src-tauri/`.
pub const SHELL_IGNORE_DIRS: &[&str] = &[
  "node_modules",
  "target",
  "dist",
  ".tatu",
  ".pnpm-store",
  "gen",
  "tests",
];

/// Files a shell snapshot never records. A lockfile is hundreds of KB of
/// resolver noise no reader ever sees. `package.json` is treated like one:
/// pnpm writes the resolved version whatever range you ask for, so recording
/// it would drift on every plugin release, and the docs render JS installs as
/// package-manager tabs anyway. (`cargo add pkg@2` writes `"2"`, so Cargo.toml
/// stays recorded.) The harness's own manifest and any `*.log` output are not
/// tutorial content either.
pub const SHELL_IGNORE_FILES: &[&str] = &[
  "Cargo.lock",
  "pnpm-lock.yaml",
  "package-lock.json",
  "yarn.lock",
  "bun.lock",
  "bun.lockb",
  "package.json",
  "tatu-test-manifest.xml",
];

// The work tree as text, keyed by slash path. Binary files are dropped: a
// snapshot is text, so a file that does not decode has nothing to record.
fn text_snapshot(work_dir: &Path) -> Result<BTreeMap<String, String>> {
  let mut files = BTreeMap::new();
  let walk = walkdir::WalkDir::new(work_dir)
    .sort_by_file_name()
    .into_iter()
    // depth 0 is the work dir itself; rejecting it would skip the whole walk
    .filter_entry(|entry| {
      entry.depth() == 0
        || !entry.file_type().is_dir()
        || !SHELL_IGNORE_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
    });
  for entry in walk {
    let entry = entry.map_err(|e| Error::Runner(e.to_string()))?;
    if !entry.file_type().is_file() {
      continue;
    }
    let name = entry.file_name().to_string_lossy().into_owned();
    if SHELL_IGNORE_FILES.contains(&name.as_str()) || name.ends_with(".log") {
      continue;
    }
    if let Ok(text) = String::from_utf8(fs::read(entry.path())?) {
      files.insert(helpers::rel_slash(work_dir, entry.path()), text);
    }
  }
  Ok(files)
}

// A shell mutation is recorded by what it did to the work tree, not by its
// command string: snapshot before and after, and the difference is the record.
pub fn apply_shell(run: &str, cwd: Option<&str>, work_dir: &Path) -> Result<Vec<Applied>> {
  let dir = match cwd {
    Some(sub) => work_dir.join(sub),
    None => work_dir.to_path_buf(),
  };
  let before_tree = text_snapshot(work_dir)?;
  let output = shell_command(run).current_dir(&dir).output()?;
  if !output.status.success() {
    return Err(Error::Runner(format!(
      "shell mutation failed ({run}):\n{}\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    )));
  }
  let after_tree = text_snapshot(work_dir)?;

  for path in before_tree.keys() {
    if !after_tree.contains_key(path) {
      return Err(Error::Runner(format!(
        "shell mutation deleted {path} — snapshots record content, not deletions; restructure the tutorial so the file survives"
      )));
    }
  }
  // BTreeMap order, so the recorded files read in path order
  let mut files = Vec::new();
  for (path, after) in after_tree {
    match before_tree.get(&path) {
      Some(before) if *before == after => {}
      before => files.push(FileChange {
        path,
        before: before.cloned(),
        after,
      }),
    }
  }
  Ok(vec![Applied::Shell {
    command: run.to_string(),
    cwd: cwd.unwrap_or(".").to_string(),
    files,
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

// Re-vendor guard: an overlay wholesale-replaces its base file, so base lines
// the recorded tutorial never removed must survive. Returns the lines the
// fresh overlay drops beyond what the recorded one did — non-empty means the
// base changed under the overlay and applying it would silently revert
// scaffold content the tutorial never discusses.
pub fn overlay_reverted_lines(
  recorded_before: &str,
  recorded_after: &str,
  fresh_before: &str,
  fresh_after: &str,
) -> Vec<String> {
  // how often each line is lost across the pair; a line that only moved is
  // still there, so it does not count. `lines()` drops a trailing \r, which
  // keeps a CRLF work tree comparable to the LF snapshots.
  fn removed_counts<'a>(before: &'a str, after: &str) -> std::collections::HashMap<&'a str, i32> {
    let mut counts: std::collections::HashMap<&str, i32> = std::collections::HashMap::new();
    for line in before.lines() {
      *counts.entry(line).or_default() += 1;
    }
    for line in after.lines() {
      if let Some(count) = counts.get_mut(line) {
        *count -= 1;
      }
    }
    counts.retain(|_, n| *n > 0);
    counts
  }
  let recorded = removed_counts(recorded_before, recorded_after);
  let mut offending: Vec<String> = removed_counts(fresh_before, fresh_after)
    .into_iter()
    .filter(|(line, n)| *n > recorded.get(line).copied().unwrap_or(0))
    .map(|(line, _)| line.to_string())
    .collect();
  offending.sort();
  offending
}

#[cfg(test)]
mod tests {
  use super::{apply_shell, deep_merge, overlay_reverted_lines, Applied};
  use std::fs;
  use std::path::{Path, PathBuf};

  // one dir per test so the suite can run in parallel; the pid keeps
  // concurrent `cargo test` invocations apart
  fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tatu-mutations-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
  }

  // one shell record carrying the command verbatim, plus the files it changed
  fn shell_files(applied: Vec<Applied>, run: &str) -> Vec<(String, Option<String>, String)> {
    let [Applied::Shell {
      command,
      cwd,
      files,
    }] = &applied[..]
    else {
      panic!("expected one shell mutation, got {applied:?}");
    };
    assert_eq!(command, run);
    assert_eq!(cwd, ".");
    files
      .iter()
      .map(|f| (f.path.clone(), f.before.clone(), f.after.clone()))
      .collect()
  }

  #[test]
  fn a_shell_mutation_records_the_files_it_wrote() {
    let dir = temp_dir("wrote");
    write(&dir, "src/a.txt", "original\n");
    let run = if cfg!(windows) {
      "echo new> src/new.txt && echo changed> src/a.txt"
    } else {
      "printf 'new\\n' > src/new.txt && printf 'changed\\n' > src/a.txt"
    };
    let files = shell_files(apply_shell(run, None, &dir).unwrap(), run);
    let paths: Vec<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(paths, ["src/a.txt", "src/new.txt"]);
    let (_, edited_before, edited_after) = &files[0];
    assert_eq!(edited_before.as_deref(), Some("original\n"));
    assert!(edited_after.contains("changed"), "{edited_after}");
    let (_, created_before, created_after) = &files[1];
    assert_eq!(created_before.as_deref(), None);
    assert!(created_after.contains("new"), "{created_after}");
    let _ = fs::remove_dir_all(&dir);
  }

  // lockfiles, dependency stores and logs are noise a reader never sees, so a
  // command that only touches them records nothing at all
  #[test]
  fn ignored_paths_are_not_recorded() {
    let dir = temp_dir("ignored");
    let run = if cfg!(windows) {
      "mkdir node_modules && echo x> node_modules/x.txt && echo y> Cargo.lock && echo z> foo.log && echo p> package.json"
    } else {
      "mkdir -p node_modules && printf 'x\\n' > node_modules/x.txt && printf 'y\\n' > Cargo.lock && printf 'z\\n' > foo.log && printf 'p\\n' > package.json"
    };
    assert!(shell_files(apply_shell(run, None, &dir).unwrap(), run).is_empty());
    let _ = fs::remove_dir_all(&dir);
  }

  #[test]
  fn a_deleted_file_is_an_error() {
    let dir = temp_dir("deleted");
    write(&dir, "src/a.txt", "original\n");
    let run = if cfg!(windows) {
      "del src\\a.txt"
    } else {
      "rm src/a.txt"
    };
    let err = apply_shell(run, None, &dir).unwrap_err().to_string();
    assert!(err.contains("deleted"), "{err}");
    assert!(err.contains("src/a.txt"), "{err}");
    let _ = fs::remove_dir_all(&dir);
  }

  // a snapshot is text; a file that does not decode has nothing to record
  #[test]
  fn a_binary_file_is_skipped() {
    let dir = temp_dir("binary");
    let work = dir.join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(dir.join("bin.dat"), [0x00u8, 0xff, 0xfe, 0x80]).unwrap();
    let run = if cfg!(windows) {
      "copy ..\\bin.dat bin.dat"
    } else {
      "cp ../bin.dat bin.dat"
    };
    assert!(shell_files(apply_shell(run, None, &work).unwrap(), run).is_empty());
    assert!(work.join("bin.dat").exists());
    let _ = fs::remove_dir_all(&dir);
  }

  const BEFORE: &str = "context\nold_line\ncontext2\n";
  const AFTER: &str = "context\nnew_line\nadded_line\ncontext2\n";

  #[test]
  fn identical_snapshots_pass() {
    assert!(overlay_reverted_lines(BEFORE, AFTER, BEFORE, AFTER).is_empty());
  }

  #[test]
  fn extra_removal_is_flagged() {
    // the base gained a line the overlay was never re-synced with
    let fresh_before = "context\nold_line\nupstream_new_line\ncontext2\n";
    assert_eq!(
      overlay_reverted_lines(BEFORE, AFTER, fresh_before, AFTER),
      vec!["upstream_new_line".to_string()]
    );
  }

  #[test]
  fn changed_additions_alone_pass() {
    // tutorial author edited what the overlay adds — not a revert
    let fresh_after = "context\ndifferent_new_line\ncontext2\n";
    assert!(overlay_reverted_lines(BEFORE, AFTER, BEFORE, fresh_after).is_empty());
  }

  // comparing multisets instead of diff text: a base that reorders a line it
  // still carries is not reverting anything
  #[test]
  fn moved_line_is_not_a_revert() {
    let fresh_before = "context2\ncontext\nold_line\n";
    let fresh_after = "context2\ncontext\nnew_line\nadded_line\n";
    assert!(overlay_reverted_lines(BEFORE, AFTER, fresh_before, fresh_after).is_empty());
  }

  // the exact greet-tutorial merge against the real pool base — the pinned
  // guarantee is json_style's minimal render, so the diff is computed here
  // rather than recorded anywhere
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
    let diff = crate::helpers::unified_diff(
      &before,
      &after,
      "a/src-tauri/capabilities/default.json",
      "b/src-tauri/capabilities/default.json",
    );
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
