// Byte-compatibility gate for the generated test templates.
//
// The fixtures under tests/fixtures/generated/ are proven output, validated
// against a real create-tauri-app scaffold; the ACL deny→grant→allow evidence
// rests on them. A wrong character here silently invalidates that evidence,
// so the template must reproduce them byte-for-byte. If you change the
// template deliberately, re-prove the greet tutorial first, then regenerate
// the fixtures with `TATU_BLESS=1 cargo test --test golden` — never edit them
// by hand.

use std::path::Path;

use tauri_tutorial_runner::harness::{generate_ipc_test_file, sanitize, IpcCase};
use tauri_tutorial_runner::tutorial::{load_tutorial, Step, Tutorial};

fn generate(tutorial: &Tutorial, step: &Step, phase: &str) -> String {
  let list = match phase {
    "pre" => &step.preconditions,
    _ => &step.assertions,
  };
  let cases: Vec<IpcCase<'_>> = list
    .iter()
    .filter(|a| a.kind.is_ipc())
    .enumerate()
    .map(|(i, a)| IpcCase {
      label: format!(
        "{phase}_{i}_{}",
        sanitize(a.command.as_deref().unwrap_or_default())
      ),
      assertion: a,
    })
    .collect();
  generate_ipc_test_file(step.harness.as_ref().or(tutorial.harness.as_ref()), &cases)
}

#[test]
fn generated_tests_match_proven_fixtures() {
  let tutorial = load_tutorial(
    Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tutorials/greet-command")
      .as_path(),
  )
  .expect("greet tutorial loads");

  let fixtures = [
    ("verify-greet", "step", "tatu_step_verify_greet.rs"),
    ("add-fs-plugin", "step", "tatu_step_add_fs_plugin.rs"),
    (
      "grant-fs-permission",
      "pre",
      "tatu_pre_grant_fs_permission.rs",
    ),
    (
      "grant-fs-permission",
      "step",
      "tatu_step_grant_fs_permission.rs",
    ),
  ];

  // TATU_BLESS=1 accepts the current template output as the new fixtures —
  // only after a live run has re-proven it; blessing an unproven template
  // just locks in the drift
  let bless = std::env::var_os("TATU_BLESS").is_some_and(|v| v != "0");
  // and never in CI, where a leaked variable would rubber-stamp that drift
  // as a green run
  assert!(
    !(bless && std::env::var_os("CI").is_some()),
    "TATU_BLESS must not be set in CI — bless locally after a live re-prove"
  );

  for (step_id, phase, fixture) in fixtures {
    let step = tutorial
      .steps
      .iter()
      .find(|s| s.id == step_id)
      .expect("fixture step exists");
    let generated = generate(&tutorial, step, phase);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tests/fixtures/generated")
      .join(fixture);
    let golden = std::fs::read_to_string(&path)
      .expect("fixture readable")
      .replace("\r\n", "\n");
    if bless && generated != golden {
      std::fs::write(&path, &generated).expect("fixture writable");
      println!("blessed {fixture} from the current template");
      continue;
    }
    assert_eq!(
      generated, golden,
      "template drifted from proven output: {fixture}"
    );
  }
}
