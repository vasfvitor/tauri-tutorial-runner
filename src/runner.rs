use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use colored::Colorize;

use crate::error::{Error, Result};
use crate::harness::{apply_harness, generate_ipc_test_file, sanitize, IpcCase};
use crate::helpers;
use crate::manifest::{platform, Manifest, MutationRecord, ResultRecord, Status, StepRecord};
use crate::mutations::{
  apply_json_merge, apply_overlay, apply_shell, overlay_reverted_lines, shell_command, Applied,
};
use crate::snapshot::{self, Snapshots};
use crate::tutorial::{Assertion, AssertionKind, Mutation, Step, Tutorial};

pub struct RunOptions {
  pub authoritative: bool,
  pub only_step: Option<String>,
}

pub fn run_tutorial(tutorial: &Tutorial, options: &RunOptions) -> Result<()> {
  // an unknown --step would filter every step out and still replace the out
  // tree with a manifest covering nothing
  if let Some(only) = &options.only_step {
    if !tutorial.steps.iter().any(|step| &step.id == only) {
      return Err(Error::Runner(format!(
        "unknown step \"{only}\" — tutorial.yaml declares [{}]",
        tutorial
          .steps
          .iter()
          .map(|step| step.id.as_str())
          .collect::<Vec<_>>()
          .join(", ")
      )));
    }
  }

  validate_seam(tutorial)?;

  let repo_root = tutorial.repo_root()?;
  let work_dir = repo_root.join(".tatu").join("work").join(&tutorial.id);
  // shared across runs so a re-check pays incremental compile cost, not a cold build
  let target_dir = repo_root
    .join(".tatu")
    .join("cargo-target")
    .join(&tutorial.id);

  if work_dir.exists() {
    fs::remove_dir_all(&work_dir)?;
  }
  fs::create_dir_all(&work_dir)?;
  helpers::copy_dir(&tutorial.fixture_dir, &work_dir)?;
  println!("work tree: {}", work_dir.display());

  let mut manifest = Manifest {
    schema: crate::manifest::SCHEMA_REF.to_string(),
    schema_version: crate::manifest::SCHEMA_VERSION,
    id: tutorial.id.clone(),
    title: tutorial.title.clone(),
    advisory: Some(!options.authoritative),
    platform: Some(platform().to_string()),
    steps: Vec::new(),
  };

  let mut failed = false;
  let mut snaps = Snapshots::default();
  let expected = ExpectedTree::load(tutorial)?;

  for step in &tutorial.steps {
    if let Some(only) = &options.only_step {
      if &step.id != only {
        continue;
      }
    }
    println!("\n== step: {}", step.id);
    let mut record = StepRecord {
      id: step.id.clone(),
      task: step.task.clone(),
      mutations: Vec::new(),
      preconditions: Vec::new(),
      assertions: Vec::new(),
    };

    record.preconditions =
      run_assertion_phase(step, &step.preconditions, "pre", &work_dir, &target_dir)?;

    for mutation in &step.mutations {
      let applied: Vec<Applied> = match mutation {
        Mutation::Overlay {} => {
          let applied = apply_overlay(tutorial, &step.id, &work_dir)?;
          check_overlay_reverts(&expected, &step.id, &applied)?;
          applied
        }
        Mutation::JsonMerge { file, merge } => apply_json_merge(file, merge, &work_dir)?,
        Mutation::Shell { run, cwd } => apply_shell(run, cwd.as_deref(), &work_dir)?,
      };
      for item in applied {
        match item {
          Applied::File {
            path,
            before,
            after,
          } => {
            let created = snaps.record_file(&step.id, &path, before.as_deref(), &after)?;
            record.mutations.push(MutationRecord {
              file: Some(path),
              created: Some(created),
              command: None,
              cwd: None,
            });
          }
          // the command first, then the files it changed — the v3 convention
          // consumers render as "run this, and here is what it did"
          Applied::Shell {
            command,
            cwd,
            files,
          } => {
            record.mutations.push(MutationRecord {
              file: None,
              created: None,
              command: Some(command),
              cwd: Some(cwd),
            });
            for change in files {
              let created = snaps.record_file(
                &step.id,
                &change.path,
                change.before.as_deref(),
                &change.after,
              )?;
              println!("   recorded {}", change.path);
              record.mutations.push(MutationRecord {
                file: Some(change.path),
                created: Some(created),
                command: None,
                cwd: None,
              });
            }
          }
        }
      }
    }
    if !record.mutations.is_empty() {
      println!("   applied {} mutation(s)", record.mutations.len());
    }
    // safety net behind HarnessGuard: runner scaffolding must never appear in
    // what readers see. A base carrying the marker means restore() did not run
    // before the next step captured the file.
    if let Some((path, _)) = snaps
      .recorded_in(&step.id)
      .iter()
      .find(|(_, content)| content.contains("tatu:harness"))
    {
      return Err(Error::Runner(format!(
        "internal: harness leaked into {path} — this is a tatu bug"
      )));
    }

    record.assertions =
      run_assertion_phase(step, &step.assertions, "step", &work_dir, &target_dir)?;

    let step_failed = record
      .preconditions
      .iter()
      .chain(record.assertions.iter())
      .any(|r| r.status == Status::Fail);
    manifest.steps.push(record);

    if step_failed {
      failed = true;
      println!("   step {}: {}", step.id, "FAIL".red());
      break; // later steps build on this state; no point continuing
    }
    println!("   step {}: {}", step.id, "ok".green());
  }

  let out_dir = tutorial.out_dir()?;
  snapshot::write_tree(&out_dir, &manifest, &snaps.tree_files())?;
  snapshot::write_run_schema(out_dir.parent().expect("out dir has a parent"))?;
  println!(
    "\nmanifest: {}{}",
    out_dir.join(snapshot::MANIFEST_FILE).display(),
    if manifest.advisory == Some(true) {
      " (advisory)"
    } else {
      ""
    }
  );

  if failed {
    return Err(Error::Runner("tutorial failed — see output above".into()));
  }
  Ok(())
}

/// Generated tests build the tutorial's own app through the `configure` seam,
/// so every lib.rs a run compiles has to expose it. Not part of
/// `load_tutorial`: `verify` and `bless` read trees, and the id-rejection
/// tests build tutorials with no scaffold at all.
pub fn validate_seam(tutorial: &Tutorial) -> Result<()> {
  const SEAM: &str = "pub fn configure<";
  let lib_rs = Path::new("src-tauri").join("src").join("lib.rs");
  let mut files = vec![tutorial.fixture_dir.join(&lib_rs)];
  for step in &tutorial.steps {
    let overlay = tutorial.dir.join("steps").join(&step.id).join(&lib_rs);
    if overlay.exists() {
      files.push(overlay);
    }
  }
  for file in files {
    let source =
      fs::read_to_string(&file).map_err(|e| Error::Validate(format!("{}: {e}", file.display())))?;
    if !source.contains(SEAM) {
      return Err(Error::Validate(format!(
        "{}: no `{SEAM}` — generated tests build the app from the seam, so every lib.rs needs `pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R>` with the plugins, handlers and setup hook chained inside it",
        file.display()
      )));
    }
  }
  Ok(())
}

/// The committed tree, indexed for the re-vendor guard: what the tutorial
/// recorded a file looking like on either side of each step. An absent
/// `expected/` dir yields an empty tree and the guard becomes a no-op — a
/// first authoring run has nothing to protect yet.
#[derive(Default)]
struct ExpectedTree {
  manifest: Option<Manifest>,
  base: BTreeMap<String, String>,
  steps: BTreeMap<(String, String), String>,
}

impl ExpectedTree {
  fn load(tutorial: &Tutorial) -> Result<Self> {
    let dir = tutorial.expected_dir();
    if !dir.is_dir() {
      return Ok(Self::default());
    }
    let tree = snapshot::read_tree(&dir)?;
    let manifest: Manifest = serde_json::from_str(&tree.manifest).map_err(|e| {
      Error::Runner(format!(
        "unreadable {}: {e}",
        dir.join(snapshot::MANIFEST_FILE).display()
      ))
    })?;
    let mut base = BTreeMap::new();
    let mut steps = BTreeMap::new();
    for (path, content) in tree.files {
      // dropping an unreadable entry would disable the guard for that file
      // alone, which is the one failure mode the guard exists to catch
      match snapshot::parse_tree_path(&path) {
        Some((None, file)) => {
          base.insert(file.to_string(), content);
        }
        Some((Some(step), file)) => {
          steps.insert((step.to_string(), file.to_string()), content);
        }
        None => {
          return Err(Error::Runner(format!(
            "unexpected entry {path} in {} — a tutorial tree holds {}, base/** and steps/**",
            dir.display(),
            snapshot::MANIFEST_FILE
          )))
        }
      }
    }
    Ok(Self {
      manifest: Some(manifest),
      base,
      steps,
    })
  }

  /// the file's content after `step`, if the tutorial recorded a mutation there
  fn after(&self, step: &str, file: &str) -> Option<&str> {
    self
      .steps
      .get(&(step.to_string(), file.to_string()))
      .map(String::as_str)
  }

  /// what a consumer derives as the file's before at `step`: the most recent
  /// snapshot from an earlier step, else the base
  fn before(&self, step: &str, file: &str) -> Option<&str> {
    let manifest = self.manifest.as_ref()?;
    let mut latest = self.base.get(file).map(String::as_str);
    for recorded in &manifest.steps {
      if recorded.id == step {
        return latest;
      }
      if let Some(content) = self.after(&recorded.id, file) {
        latest = Some(content);
      }
    }
    None
  }
}

// the re-vendor guard: hard-fail before assertions ever run if an overlay
// would silently revert base content the recorded tutorial never removed
// (a recurring docs failure: a guide written against an older scaffold
// quietly undoes what the newer scaffold added)
fn check_overlay_reverts(
  expected: &ExpectedTree,
  step_id: &str,
  applied: &[Applied],
) -> Result<()> {
  for item in applied {
    let Applied::File {
      path,
      before: Some(fresh_before),
      after,
    } = item
    else {
      continue;
    };
    // no recorded snapshot for this (step, file) — a new overlay file, nothing
    // to protect yet
    let Some(recorded_after) = expected.after(step_id, path) else {
      continue;
    };
    // a recorded created file has no base/<file>; an empty before still trips
    // the guard on anything the fresh overlay would remove
    let recorded_before = expected.before(step_id, path).unwrap_or_default();
    let reverted = overlay_reverted_lines(recorded_before, recorded_after, fresh_before, after);
    if !reverted.is_empty() {
      return Err(Error::Runner(format!(
        "re-vendor guard: overlay steps/{step_id}/{path} would revert base lines expected/steps/{step_id}/{path} never removed:\n  -{}\nthe base changed under this overlay — re-sync the overlay with the new base, then re-bless the tutorial",
        reverted.join("\n  -")
      )));
    }
  }
  Ok(())
}

fn run_assertion_phase(
  step: &Step,
  list: &[Assertion],
  phase: &str,
  work_dir: &Path,
  target_dir: &Path,
) -> Result<Vec<ResultRecord>> {
  let mut results = Vec::new();
  if list.is_empty() {
    return Ok(results);
  }

  let ipc: Vec<&Assertion> = list.iter().filter(|a| a.kind.is_ipc()).collect();
  if !ipc.is_empty() {
    let guard = apply_harness(work_dir)?;
    let test_name = format!("tatu_{phase}_{}", sanitize(&step.id));
    let cases: Vec<IpcCase<'_>> = ipc
      .iter()
      .enumerate()
      .map(|(i, a)| IpcCase {
        label: format!(
          "{phase}_{i}_{}",
          sanitize(a.command.as_deref().unwrap_or_default())
        ),
        assertion: a,
      })
      .collect();
    let source = generate_ipc_test_file(&guard.lib_name, &cases);
    let test_path = work_dir
      .join("src-tauri")
      .join("tests")
      .join(format!("{test_name}.rs"));
    fs::write(&test_path, source)?;

    // one process per case: a phase-wide run would stamp a single boolean onto
    // every assertion, and process-global plugins (log) would collide
    let build = cargo_build_tests(work_dir, target_dir, &test_name)?;
    let mut all_passed = build.ok;
    if build.ok {
      for (case, assertion) in cases.iter().zip(ipc.iter()) {
        let fn_name = sanitize(&case.label);
        let outcome = cargo_run_case(work_dir, target_dir, &test_name, &fn_name)?;
        all_passed &= outcome.ok;
        if outcome.ok {
          println!("   {fn_name}: {} ({}s)", "pass".green(), outcome.seconds);
        } else {
          // the panic lives in the test harness's stdout; cargo's stderr is
          // mostly compile noise — tail them separately so neither drowns the
          // other
          println!("   | --- {fn_name} (stdout)");
          println!("{}", helpers::last_lines(&outcome.stdout, 40, "   | "));
          println!("   | --- cargo (stderr)");
          println!("{}", helpers::last_lines(&outcome.stderr, 15, "   | "));
        }
        results.push(ResultRecord {
          kind: assertion.kind,
          command: assertion.command.clone(),
          run: None,
          status: if outcome.ok {
            Status::Pass
          } else {
            Status::Fail
          },
        });
      }
    } else {
      // nothing ran, so no case has a status of its own
      println!("   | --- cargo (stderr)");
      println!("{}", helpers::last_lines(&build.stderr, 15, "   | "));
      for assertion in &ipc {
        results.push(ResultRecord {
          kind: assertion.kind,
          command: assertion.command.clone(),
          run: None,
          status: Status::Fail,
        });
      }
    }
    guard.restore(all_passed.then_some(test_path.as_path()))?;
  }

  for assertion in list.iter().filter(|a| a.kind == AssertionKind::Shell) {
    let run = assertion.run.as_deref().expect("validated: shell has run");
    let output = shell_command(run).current_dir(work_dir).output()?;
    let ok = output.status.success();
    results.push(ResultRecord {
      kind: AssertionKind::Shell,
      command: None,
      run: Some(run.to_string()),
      status: if ok { Status::Pass } else { Status::Fail },
    });
    if !ok {
      let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
      );
      println!("{}", helpers::last_lines(&text, 40, "   | "));
    }
  }

  Ok(results)
}

struct TestOutcome {
  ok: bool,
  stdout: String,
  stderr: String,
  seconds: u64,
}

// Same CARGO_TARGET_DIR and args across both invocations so the per-case runs
// reuse the build instead of relinking. No shell: fn names are [A-Za-z0-9_].
fn cargo_test_run(work_dir: &Path, target_dir: &Path, args: &[&str]) -> Result<TestOutcome> {
  let started = Instant::now();
  let output = std::process::Command::new("cargo")
    .args(args)
    .current_dir(work_dir.join("src-tauri"))
    .env("CARGO_TARGET_DIR", target_dir)
    .output()?;
  Ok(TestOutcome {
    ok: output.status.success(),
    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    seconds: started.elapsed().as_secs(),
  })
}

/// Compile the phase's test binary once — a compile error belongs to the phase,
/// not to any one case.
fn cargo_build_tests(work_dir: &Path, target_dir: &Path, test_name: &str) -> Result<TestOutcome> {
  cargo_test_run(
    work_dir,
    target_dir,
    &["test", "--test", test_name, "--no-run"],
  )
}

/// Run one case in its own process. libtest exits 0 with `running 0 tests`
/// when `--exact` matches nothing, so a renamed case would pass on the exit
/// status alone — the `1 passed` count is what proves it ran.
fn cargo_run_case(
  work_dir: &Path,
  target_dir: &Path,
  test_name: &str,
  fn_name: &str,
) -> Result<TestOutcome> {
  let mut outcome = cargo_test_run(
    work_dir,
    target_dir,
    &["test", "--test", test_name, "--", "--exact", fn_name],
  )?;
  outcome.ok &= outcome.stdout.contains("1 passed");
  Ok(outcome)
}

#[cfg(test)]
mod tests {
  use super::{check_overlay_reverts, validate_seam, ExpectedTree};
  use crate::manifest::Manifest;
  use crate::mutations::Applied;
  use crate::tutorial::load_tutorial;
  use std::collections::BTreeMap;
  use std::fs;
  use std::path::PathBuf;

  /// a tutorial dir with a base and one overlay step, both carrying a lib.rs
  struct TempSeam(PathBuf);

  impl TempSeam {
    fn new(label: &str, base_lib: &str, overlay_lib: &str) -> Self {
      let dir = std::env::temp_dir().join(format!("tatu-seam-{label}-{}", std::process::id()));
      let _ = fs::remove_dir_all(&dir);
      let lib_rs = PathBuf::from("src-tauri").join("src").join("lib.rs");
      for (root, source) in [
        (dir.join("base"), base_lib),
        (dir.join("steps").join("one"), overlay_lib),
      ] {
        let path = root.join(&lib_rs);
        fs::create_dir_all(path.parent().expect("lib.rs has a parent")).expect("temp dir");
        fs::write(&path, source).expect("write lib.rs");
      }
      fs::write(
        dir.join("tutorial.yaml"),
        "id: t\ntitle: T\nbase:\n  fixture: base\nsteps:\n  - id: one\n    task: do it\n    mutations:\n      - engine: overlay\n",
      )
      .expect("write tutorial.yaml");
      Self(dir)
    }
  }

  impl Drop for TempSeam {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  const SEAM: &str = "pub fn configure<R: tauri::Runtime>(b: tauri::Builder<R>) {}\n";

  #[test]
  fn the_seam_in_base_and_overlay_validates() {
    let temp = TempSeam::new("ok", SEAM, SEAM);
    validate_seam(&load_tutorial(&temp.0).expect("loads")).expect("both carry the seam");
  }

  #[test]
  fn an_overlay_lib_rs_without_the_seam_is_rejected() {
    let temp = TempSeam::new("overlay", SEAM, "pub fn run() {}\n");
    let err = validate_seam(&load_tutorial(&temp.0).expect("loads"))
      .unwrap_err()
      .to_string();
    assert!(err.contains("steps"), "{err}");
    assert!(err.contains("pub fn configure<R: tauri::Runtime>"), "{err}");
  }

  #[test]
  fn a_base_lib_rs_without_the_seam_is_rejected() {
    let temp = TempSeam::new("base", "pub fn run() {}\n", SEAM);
    let err = validate_seam(&load_tutorial(&temp.0).expect("loads"))
      .unwrap_err()
      .to_string();
    assert!(err.contains("base"), "{err}");
  }

  /// a tree recording one created file — the case with no `base/<file>`
  fn expected(step: &str, file: &str, after: &str) -> ExpectedTree {
    let manifest: Manifest = serde_json::from_str(&format!(
      r#"{{"schemaVersion":3,"id":"t","title":"T","steps":[{{"id":"{step}","task":"",
       "mutations":[{{"file":"{file}","created":true}}],"preconditions":[],"assertions":[]}}]}}"#
    ))
    .unwrap();
    let mut steps = BTreeMap::new();
    steps.insert((step.to_string(), file.to_string()), after.to_string());
    ExpectedTree {
      manifest: Some(manifest),
      base: BTreeMap::new(),
      steps,
    }
  }

  fn overlaid(path: &str, before: &str, after: &str) -> Vec<Applied> {
    vec![Applied::File {
      path: path.to_string(),
      before: Some(before.to_string()),
      after: after.to_string(),
    }]
  }

  // the file the tutorial created now exists in the re-vendored base: the
  // overlay overwrites it, so the guard has to run with an empty before
  #[test]
  fn a_created_file_still_trips_the_revert_guard() {
    let err = check_overlay_reverts(
      &expected("one", "src/app.ts", "line_a\nline_b\n"),
      "one",
      &overlaid(
        "src/app.ts",
        "line_a\nupstream\nline_b\n",
        "line_a\nline_b\n",
      ),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("re-vendor guard"), "{err}");
    assert!(err.contains("upstream"), "{err}");
  }

  #[test]
  fn an_unrecorded_overlay_file_is_not_guarded() {
    check_overlay_reverts(
      &expected("one", "src/app.ts", "x\n"),
      "one",
      &overlaid("src/other.ts", "gone\n", ""),
    )
    .unwrap();
  }
}
