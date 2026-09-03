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

fn generate(step: &Step, phase: &str) -> String {
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
  // the pool bases all scaffold as tatu-app, whose lib is tatu_app_lib
  generate_ipc_test_file("tatu_app_lib", &cases)
}

fn check_fixtures(tutorial: &Tutorial, fixtures: &[(&str, &str, &str)], bless: bool) {
  for (step_id, phase, fixture) in fixtures {
    let step = tutorial
      .steps
      .iter()
      .find(|s| s.id == *step_id)
      .expect("fixture step exists");
    let generated = generate(step, phase);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tests/fixtures/generated")
      .join(fixture);
    let golden = match std::fs::read_to_string(&path) {
      Ok(s) => s.replace("\r\n", "\n"),
      // a fixture being added for the first time only exists under bless; any
      // other read failure is a broken fixture, not a new one
      Err(e) if bless && e.kind() == std::io::ErrorKind::NotFound => String::new(),
      Err(e) => panic!("fixture {fixture} unreadable: {e}"),
    };
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

#[test]
fn generated_tests_match_proven_fixtures() {
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

  let greet = load_tutorial(
    Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tutorials/greet-command")
      .as_path(),
  )
  .expect("greet tutorial loads");
  check_fixtures(
    &greet,
    &[
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
    ],
    bless,
  );

  // plugin-store's permissions phases lock the `succeeds` expect block and a
  // plugin command denied before its capability is granted
  let store = load_tutorial(
    Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("tutorials/plugin-store")
      .as_path(),
  )
  .expect("plugin-store tutorial loads");
  check_fixtures(
    &store,
    &[
      ("permissions", "pre", "tatu_pre_store_permissions.rs"),
      ("permissions", "step", "tatu_step_store_permissions.rs"),
    ],
    bless,
  );
}
