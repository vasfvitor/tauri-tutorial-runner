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
use crate::tutorial::{Assertion, AssertionKind, Harness, Mutation, Step, Tutorial};

pub struct RunOptions {
  pub authoritative: bool,
  pub only_step: Option<String>,
}

pub fn run_tutorial(tutorial: &Tutorial, options: &RunOptions) -> Result<()> {
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

    record.preconditions = run_assertion_phase(
      tutorial,
      step,
      &step.preconditions,
      "pre",
      &work_dir,
      &target_dir,
    )?;

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
        record.mutations.push(match item {
          Applied::File {
            path,
            before,
            after,
          } => {
            let created = snaps.record_file(&step.id, &path, before.as_deref(), &after)?;
            MutationRecord {
              file: Some(path),
              created: Some(created),
              command: None,
              cwd: None,
            }
          }
          Applied::Shell { command, cwd } => MutationRecord {
            file: None,
            created: None,
            command: Some(command),
            cwd: Some(cwd),
          },
        });
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

    record.assertions = run_assertion_phase(
      tutorial,
      step,
      &step.assertions,
      "step",
      &work_dir,
      &target_dir,
    )?;

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
      if let Some(file) = path.strip_prefix("base/") {
        base.insert(file.to_string(), content);
      } else if let Some((step, file)) = path
        .strip_prefix("steps/")
        .and_then(|rest| rest.split_once('/'))
      {
        steps.insert((step.to_string(), file.to_string()), content);
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
    // no recorded pair for this (step, file) — a new overlay file, nothing to
    // protect yet
    let (Some(recorded_before), Some(recorded_after)) = (
      expected.before(step_id, path),
      expected.after(step_id, path),
    ) else {
      continue;
    };
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
  tutorial: &Tutorial,
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
    let harness = Harness::resolve(step.harness.as_ref(), tutorial.harness.as_ref());
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
    let source = generate_ipc_test_file(harness.as_ref(), &cases);
    let test_path = work_dir
      .join("src-tauri")
      .join("tests")
      .join(format!("{test_name}.rs"));
    fs::write(&test_path, source)?;

    let outcome = cargo_test(work_dir, target_dir, &test_name)?;
    guard.restore(outcome.ok.then_some(test_path.as_path()))?;
    for assertion in &ipc {
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
    if outcome.ok {
      println!("   {test_name}: {} ({}s)", "pass".green(), outcome.seconds);
    } else {
      // the panic lives in the test harness's stdout; cargo's stderr is mostly
      // compile noise — tail them separately so neither drowns the other
      println!("   | --- test output (stdout)");
      println!("{}", helpers::last_lines(&outcome.stdout, 40, "   | "));
      println!("   | --- cargo (stderr)");
      println!("{}", helpers::last_lines(&outcome.stderr, 15, "   | "));
    }
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

fn cargo_test(work_dir: &Path, target_dir: &Path, test_name: &str) -> Result<TestOutcome> {
  let started = Instant::now();
  let output = std::process::Command::new("cargo")
    .args(["test", "--test", test_name])
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
