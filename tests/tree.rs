// The tutorial tree is the committed contract: what `tatu bless` writes has to
// read back unchanged, and `tatu verify` has to report every way two trees can
// differ — a bucket that stays quiet is drift nobody sees.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tauri_tutorial_runner::manifest::{compare_trees, normalize_tree, Manifest, SCHEMA_REF};
use tauri_tutorial_runner::snapshot::{
  read_tree, write_run_schema, write_tree, Snapshots, Tree, SCHEMA_FILE,
};

const MANIFEST: &str = r#"{"schemaVersion":3,"id":"t","title":"T","advisory":true,
  "platform":"win32","steps":[{"id":"one","task":"","preconditions":[],"assertions":[],
  "mutations":[{"file":"src/lib.rs","created":false}]}]}"#;

struct TempDir(PathBuf);

impl TempDir {
  fn new(label: &str) -> Self {
    let dir = std::env::temp_dir().join(format!("tatu-tree-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    Self(dir)
  }

  fn root(&self) -> PathBuf {
    self.0.join("expected")
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn manifest() -> Manifest {
  serde_json::from_str(MANIFEST).expect("manifest parses")
}

fn snapshots() -> Snapshots {
  let mut snaps = Snapshots::default();
  snaps
    .record_file("one", "src/lib.rs", Some("old\n"), "new\n")
    .expect("first record");
  snaps
}

fn tree(manifest: &str, files: &[(&str, &str)]) -> Tree {
  Tree {
    manifest: manifest.to_string(),
    files: files
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
  }
}

#[test]
fn a_written_tree_reads_back_unchanged() {
  let temp = TempDir::new("roundtrip");
  let snaps = snapshots();
  write_tree(&temp.root(), &manifest(), &snaps.tree_files()).expect("write");

  let read = read_tree(&temp.root()).expect("read");
  assert_eq!(read.files, snaps.tree_files());
  // a committed tree carries no schema copy — only a run's out dir gets one
  assert!(!temp.path().join(SCHEMA_FILE).exists());
  write_run_schema(temp.path()).expect("run schema");
  assert!(temp.path().join(SCHEMA_FILE).exists());

  let normalized = normalize_tree(read).expect("normalize");
  assert!(!normalized.manifest.contains("advisory"));
  assert!(normalized.manifest.contains(SCHEMA_REF));
  assert_eq!(
    normalize_tree(normalized.clone())
      .expect("normalize")
      .manifest,
    normalized.manifest
  );
}

// a bless must not leave a snapshot from the run it replaces behind
#[test]
fn writing_replaces_the_previous_tree() {
  let temp = TempDir::new("replace");
  write_tree(&temp.root(), &manifest(), &snapshots().tree_files()).expect("write");
  let stale = temp.root().join("steps/one/src/stale.rs");
  fs::create_dir_all(stale.parent().expect("has a dir")).expect("mkdir");
  fs::write(&stale, "stale\n").expect("write stale");

  write_tree(&temp.root(), &manifest(), &snapshots().tree_files()).expect("rewrite");
  assert!(!stale.exists());
}

#[test]
fn an_entry_outside_the_tree_layout_is_rejected() {
  let temp = TempDir::new("stray");
  write_tree(&temp.root(), &manifest(), &snapshots().tree_files()).expect("write");
  fs::write(temp.root().join("notes.md"), "hand-dropped\n").expect("write stray");

  let err = read_tree(&temp.root())
    .expect_err("stray entry")
    .to_string();
  assert!(err.contains("unexpected entry notes.md"), "{err}");
}

#[test]
fn a_manifest_change_is_reported_as_a_diff() {
  let expected = tree("{\"id\":\"t\"}\n", &[]);
  let fresh = tree("{\"id\":\"u\"}\n", &[]);
  let report = compare_trees(&expected, &fresh);
  assert!(report.contains("expected/manifest.json"), "{report}");
  assert!(report.contains("+{\"id\":\"u\"}"), "{report}");
}

#[test]
fn snapshot_set_changes_are_reported_as_added_and_removed() {
  let expected = tree(
    "{}\n",
    &[("base/a.ts", "a\n"), ("steps/one/gone.ts", "g\n")],
  );
  let fresh = tree("{}\n", &[("base/a.ts", "a\n"), ("steps/one/new.ts", "n\n")]);
  let report = compare_trees(&expected, &fresh);
  assert!(report.contains("  added     steps/one/new.ts"), "{report}");
  assert!(report.contains("  removed   steps/one/gone.ts"), "{report}");
}

#[test]
fn a_changed_shared_snapshot_is_reported_per_file() {
  let expected = tree("{}\n", &[("steps/one/src/lib.rs", "old\n")]);
  let fresh = tree("{}\n", &[("steps/one/src/lib.rs", "new\n")]);
  let report = compare_trees(&expected, &fresh);
  assert!(
    report.contains("--- expected/steps/one/src/lib.rs"),
    "{report}"
  );
  assert!(report.contains("-old"), "{report}");
  assert!(report.contains("+new"), "{report}");
}

#[test]
fn identical_trees_report_nothing() {
  let files: BTreeMap<String, String> = snapshots().tree_files();
  let one = Tree {
    manifest: MANIFEST.to_string(),
    files: files.clone(),
  };
  let other = Tree {
    manifest: MANIFEST.to_string(),
    files,
  };
  assert_eq!(compare_trees(&one, &other), "");
}
