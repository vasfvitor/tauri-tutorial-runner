use std::fs;
use std::path::Path;
use std::time::Instant;

use colored::Colorize;

use crate::error::{Error, Result};
use crate::harness::{apply_harness, generate_ipc_test_file, sanitize, IpcCase};
use crate::helpers;
use crate::manifest::{platform, Manifest, MutationRecord, ResultRecord, Status, StepRecord};
use crate::mutations::{apply_json_merge, apply_overlay, apply_shell, shell_command};
use crate::tutorial::{Assertion, AssertionKind, Mutation, Step, Tutorial};

pub struct RunOptions {
  pub authoritative: bool,
  pub only_step: Option<String>,
}

pub fn run_tutorial(tutorial: &Tutorial, options: &RunOptions) -> Result<()> {
  let repo_root = tutorial
    .dir
    .parent()
    .and_then(Path::parent)
    .ok_or_else(|| Error::Runner("tutorial dir has no repo root two levels up".into()))?
    .to_path_buf();
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
    schema_version: crate::manifest::SCHEMA_VERSION,
    id: tutorial.id.clone(),
    title: tutorial.title.clone(),
    advisory: !options.authoritative,
    platform: platform().to_string(),
    steps: Vec::new(),
  };

  let mut failed = false;

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
      let applied: Vec<MutationRecord> = match mutation {
        Mutation::Overlay {} => apply_overlay(tutorial, &step.id, &work_dir)?,
        Mutation::JsonMerge { file, merge } => apply_json_merge(file, merge, &work_dir)?,
        Mutation::Shell { run, cwd } => apply_shell(run, cwd.as_deref(), &work_dir)?,
      };
      record.mutations.extend(applied);
    }
    if !record.mutations.is_empty() {
      println!("   applied {} mutation(s)", record.mutations.len());
    }
    // safety net behind HarnessGuard: runner scaffolding must never appear in
    // the diffs readers see
    if record.mutations.iter().any(|m| {
      m.diff
        .as_deref()
        .is_some_and(|d| d.contains("tatu:harness"))
    }) {
      return Err(Error::Runner(
        "internal: harness leaked into a recorded diff — this is a tatu bug".into(),
      ));
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

  let out_dir = repo_root.join(".tatu").join("out").join(&tutorial.id);
  fs::create_dir_all(&out_dir)?;
  let manifest_path = out_dir.join("tutorial.manifest.json");
  fs::write(
    &manifest_path,
    serde_json::to_string_pretty(&manifest)? + "\n",
  )?;
  println!(
    "\nmanifest: {}{}",
    manifest_path.display(),
    if manifest.advisory { " (advisory)" } else { "" }
  );

  if failed {
    return Err(Error::Runner("tutorial failed — see output above".into()));
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

  for assertion in list.iter().filter(|a| !a.kind.implemented()) {
    println!(
      "   skip ({} not implemented in v0)",
      assertion.kind.as_str()
    );
    results.push(ResultRecord {
      kind: assertion.kind.as_str().to_string(),
      command: None,
      run: None,
      status: Status::Skipped,
    });
  }

  let ipc: Vec<&Assertion> = list.iter().filter(|a| a.kind.is_ipc()).collect();
  if !ipc.is_empty() {
    let guard = apply_harness(work_dir)?;
    let test_name = format!("tatu_{phase}_{}", sanitize(&step.id));
    let harness = step.harness.as_ref().or(tutorial.harness.as_ref());
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
    let source = generate_ipc_test_file(harness, &cases);
    let test_path = work_dir
      .join("src-tauri")
      .join("tests")
      .join(format!("{test_name}.rs"));
    fs::write(&test_path, source)?;

    let outcome = cargo_test(work_dir, target_dir, &test_name)?;
    guard.restore(outcome.ok.then_some(test_path.as_path()))?;
    for assertion in &ipc {
      results.push(ResultRecord {
        kind: assertion.kind.as_str().to_string(),
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
      kind: "shell".to_string(),
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
