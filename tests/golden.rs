// Byte-compatibility gate for the generated test templates.
//
// The fixtures under tests/fixtures/generated/ are proven output, validated
// against a real create-tauri-app scaffold; the ACL deny→grant→allow evidence
// rests on them. A wrong character here silently invalidates that evidence,
// so the template must reproduce them byte-for-byte. If you change the
// template deliberately, re-prove the greet tutorial first, then regenerate
// the fixtures from the work tree — never edit them by hand.

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

  for (step_id, phase, fixture) in fixtures {
    let step = tutorial
      .steps
      .iter()
      .find(|s| s.id == step_id)
      .expect("fixture step exists");
    let generated = generate(&tutorial, step, phase);
    let golden = std::fs::read_to_string(
      Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/generated")
        .join(fixture),
    )
    .expect("fixture readable")
    .replace("\r\n", "\n");
    assert_eq!(
      generated, golden,
      "template drifted from proven output: {fixture}"
    );
  }
}
