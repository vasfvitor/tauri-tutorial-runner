use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::tutorial::{Assertion, Expect, ExpectKeyword};

// Test-harness pieces the runner owns, injected into the work tree and never
// visible in the tutorial's own files. Re-applied before every assertion phase
// because an overlay may have replaced Cargo.toml or build.rs in between.
// Two landmines:
//  - tauri::test is behind the `test` feature (dev-dependency)
//  - on Windows the test exe needs the comctl32 v6 manifest, which only
//    rustc-link-arg-tests can attach, and only to integration-test targets
//
// The generated-file templates are locked byte-for-byte by tests/golden.rs;
// do not reformat them.

const BUILD_RS_BLOCK: &str = r#"// tatu:harness — cargo test exes need the comctl32 v6 manifest on Windows
// (tauri-build embeds it for bins only; without this the test exe dies at load
// with STATUS_ENTRYPOINT_NOT_FOUND on TaskDialogIndirect)
#[cfg(windows)]
fn tatu_test_manifest() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tatu-test-manifest.xml");
    println!("cargo::rustc-link-arg-tests=/MANIFEST:EMBED");
    println!("cargo::rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
}
#[cfg(not(windows))]
fn tatu_test_manifest() {}
"#;

const MANIFEST_XML: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

/// The harness is a revertible layer, never part of the canonical work tree:
/// apply it for a cargo-test phase, restore afterwards. If it stayed applied,
/// the next overlay (authored against the pristine scaffold) would record a
/// diff deleting the runner's own scaffolding — a harness leak in the manifest
/// readers see.
pub struct HarnessGuard {
  cargo_toml: std::path::PathBuf,
  cargo_toml_original: String,
  build_rs: std::path::PathBuf,
  build_rs_original: String,
  manifest_xml: std::path::PathBuf,
  /// the tutorial crate's lib name — generated tests link it to reach
  /// `configure`, and it is not always the package name
  pub lib_name: String,
}

// The base or an overlay may carry its own [dev-dependencies] (even its own
// tauri dev-dep), and appending a second table is a cargo error — so merge.
// The write is transient (HarnessGuard restores the original), so formatting
// only has to stay valid, not pretty.
fn cargo_toml_with_harness(original: &str) -> Result<String> {
  use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

  let mut doc: DocumentMut = original
    .parse()
    .map_err(|e| Error::Runner(format!("src-tauri/Cargo.toml: {e}")))?;
  let dev = doc
    .entry("dev-dependencies")
    .or_insert(Item::Table(Table::new()))
    .as_table_like_mut()
    .ok_or_else(|| {
      Error::Runner("src-tauri/Cargo.toml: [dev-dependencies] is not a table".into())
    })?;

  // absent and plain `tauri = "x"` both end as a fresh inline table (widened
  // so the feature fits); a table-shaped dep just gains the feature
  let plain_version = match dev.get("tauri") {
    None => Some("2".to_string()),
    Some(item) => item.as_str().map(str::to_string),
  };
  if let Some(version) = plain_version {
    let mut dep = InlineTable::new();
    dep.insert("version", version.into());
    dep.insert("features", Value::Array(Array::from_iter(["test"])));
    dev.insert("tauri", toml_edit::value(dep));
  } else if let Some(dep) = dev.get_mut("tauri").and_then(Item::as_table_like_mut) {
    let features = dep
      .entry("features")
      .or_insert(toml_edit::value(Array::new()))
      .as_array_mut()
      .ok_or_else(|| {
        Error::Runner(
          "src-tauri/Cargo.toml: dev-dependencies.tauri.features is not an array".into(),
        )
      })?;
    if !features.iter().any(|v| v.as_str() == Some("test")) {
      features.push("test");
    }
  } else {
    return Err(Error::Runner(
      "src-tauri/Cargo.toml: dev-dependencies.tauri has an unrecognized shape".into(),
    ));
  }

  // marker: greppable for the idempotence check and the manifest leak
  // assertion, so it must land for every shape — a `[dev-dependencies.tauri]`
  // table is not a Value, and there the features array carries it
  let tauri = dev.get_mut("tauri").expect("inserted or merged above");
  let marked = if tauri.is_value() {
    tauri.as_value_mut()
  } else {
    tauri
      .as_table_like_mut()
      .and_then(|dep| dep.get_mut("features"))
      .and_then(Item::as_value_mut)
  };
  match marked {
    Some(value) => value
      .decor_mut()
      .set_suffix(" # tatu:harness — tauri::test is feature-gated"),
    None => {
      return Err(Error::Runner(
        "internal: could not attach the tatu:harness marker — this is a tatu bug".into(),
      ))
    }
  }

  Ok(doc.to_string())
}

// cargo derives an absent [lib] name from the package name with `-` → `_`;
// generated tests need the name they will `extern crate`, not the package's.
fn lib_name(cargo_toml: &str) -> Result<String> {
  let doc: toml_edit::DocumentMut = cargo_toml
    .parse()
    .map_err(|e| Error::Runner(format!("src-tauri/Cargo.toml: {e}")))?;
  if let Some(name) = doc
    .get("lib")
    .and_then(|lib| lib.get("name"))
    .and_then(|name| name.as_str())
  {
    return Ok(name.to_string());
  }
  doc
    .get("package")
    .and_then(|package| package.get("name"))
    .and_then(|name| name.as_str())
    .map(|name| name.replace('-', "_"))
    .ok_or_else(|| {
      Error::Runner(
        "src-tauri/Cargo.toml: neither [lib] name nor [package] name — generated tests cannot name the tutorial crate".into(),
      )
    })
}

pub fn apply_harness(work_dir: &Path) -> Result<HarnessGuard> {
  let src_tauri = work_dir.join("src-tauri");

  let cargo_toml = src_tauri.join("Cargo.toml");
  let cargo_toml_original = fs::read_to_string(&cargo_toml)?;
  let lib_name = lib_name(&cargo_toml_original)?;
  if !cargo_toml_original.contains("tatu:harness") {
    fs::write(&cargo_toml, cargo_toml_with_harness(&cargo_toml_original)?)?;
  }

  let build_rs = src_tauri.join("build.rs");
  let build_rs_original = fs::read_to_string(&build_rs)?;
  if !build_rs_original.contains("tatu:harness") {
    if !build_rs_original.contains("fn main() {") {
      return Err(Error::Runner(
        "src-tauri/build.rs has no `fn main() {` to anchor the tatu test-manifest hook — without it Windows test exes die at load".into(),
      ));
    }
    let wrapped =
      build_rs_original.replacen("fn main() {", "fn main() {\n    tatu_test_manifest();", 1);
    fs::write(&build_rs, format!("{BUILD_RS_BLOCK}\n{wrapped}"))?;
  }

  let manifest_xml = src_tauri.join("tatu-test-manifest.xml");
  fs::write(&manifest_xml, MANIFEST_XML)?;

  fs::create_dir_all(src_tauri.join("tests"))?;

  Ok(HarnessGuard {
    cargo_toml,
    cargo_toml_original,
    build_rs,
    build_rs_original,
    manifest_xml,
    lib_name,
  })
}

impl HarnessGuard {
  /// Restore the work tree to its harness-free state. The generated test file
  /// is removed only on success — on failure it stays for debugging.
  pub fn restore(self, remove_test: Option<&Path>) -> Result<()> {
    fs::write(&self.cargo_toml, &self.cargo_toml_original)?;
    fs::write(&self.build_rs, &self.build_rs_original)?;
    let _ = fs::remove_file(&self.manifest_xml);
    if let Some(test) = remove_test {
      let _ = fs::remove_file(test);
    }
    Ok(())
  }
}

pub struct IpcCase<'a> {
  pub label: String,
  pub assertion: &'a Assertion,
}

// One generated integration-test file per assertion phase: a process can only
// host one Tauri app, so each phase gets its own test binary. The app under
// test is the tutorial's own, reached through the `configure` seam
// `runner::validate_seam` requires — so a step that forgets to register a
// plugin fails here instead of passing against a stand-in.
const TEST_PRELUDE: &str = r#"// generated by tatu — do not edit; regenerated on every run
#![allow(unused_imports, dead_code, deprecated)]
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::Manager;

// the tutorial's own app on the mock runtime; run_iteration is the one way to
// run the setup hook and create the config windows without an event loop
fn build_app() -> tauri::App<MockRuntime> {
    let mut app = {lib}::configure(mock_builder())
        .build(tauri::generate_context!())
        .expect("failed to build the tutorial app");
    app.run_iteration(|_, _| {});
    app
}

// the config windows already exist after build_app; create `main` only when
// the config declares none
fn main_webview(app: &tauri::App<MockRuntime>) -> tauri::WebviewWindow<MockRuntime> {
    match app.get_webview_window("main") {
        Some(webview) => webview,
        None => tauri::WebviewWindowBuilder::new(app, "main", Default::default())
            .build()
            .unwrap(),
    }
}

// one invoke over the webview's own URL (devUrl under `cargo test`) — the one
// origin that is Origin::Local on every platform; a hardcoded
// http://tauri.localhost is remote (= fully ACL-denied) off Windows.
// Also callable from `setup:` snippets so a stateful sequence stays in the app.
fn tatu_invoke(
    webview: &tauri::WebviewWindow<MockRuntime>,
    cmd: &str,
    args: &str,
) -> Result<InvokeResponseBody, serde_json::Value> {
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: webview.url().unwrap(),
            body: InvokeBody::Json(serde_json::from_str(args).unwrap()),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}
"#;

pub fn generate_ipc_test_file(lib_name: &str, cases: &[IpcCase<'_>]) -> String {
  let tests = cases
    .iter()
    .map(|case| {
      let fn_name = sanitize(&case.label);
      let command = json_string(case.assertion.command.as_deref().unwrap_or_default());
      let args = case
        .assertion
        .args
        .clone()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
      let setup = match &case.assertion.setup {
        Some(setup) => indent(setup, 4),
        None => String::new(),
      };
      format!(
        r##"#[test]
fn {fn_name}() {{
    let app = build_app();
    let webview = main_webview(&app);
{setup}    let res = tatu_invoke(&webview, {command}, r#"{args}"#);
{expect}
}}"##,
        args = serde_json::to_string(&args).expect("args serialize"),
        expect = expect_block(
          case
            .assertion
            .expect
            .as_ref()
            .expect("validated: ipc has expect")
        ),
      )
    })
    .collect::<Vec<_>>()
    .join("\n\n");

  format!("{}\n{tests}\n", TEST_PRELUDE.replace("{lib}", lib_name))
}

// Raw is the plugin-byte-response case (e.g. fs read_text_file); Json is the
// common command-return case — both normalize to text before comparing
const NORMALIZE: &str = r#"    let body = res.expect("expected the command to succeed");
    let text = match body {
        tauri::ipc::InvokeResponseBody::Raw(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        tauri::ipc::InvokeResponseBody::Json(s) => s,
    };"#;

fn expect_block(expect: &Expect) -> String {
  match expect {
    // "not allowed" alone also matches "Command not found" and the http
    // plugin's scope refusal — an ACL denial is one of the shapes
    // RuntimeAuthority::resolve_access_message emits, nothing else
    Expect::Keyword(ExpectKeyword::Denied) => {
      r#"    let err = res.expect_err("expected an ACL denial");
    let msg = err.to_string();
    assert!(
        !msg.contains("Command not found") && !msg.contains("Plugin not found"),
        "the command or plugin does not exist, which is not an ACL denial: {msg}"
    );
    assert!(
        msg.contains("Permissions associated with this command")
            || msg.contains("explicitly denied on origin")
            || msg.contains("not allowed on window")
            || msg.contains("not allowed on origin [")
            || msg.contains("not allowed by ACL"),
        "expected an ACL denial, got: {msg}"
    );"#
        .to_string()
    }
    Expect::Keyword(ExpectKeyword::Succeeds) => {
      r#"    res.expect("expected the command to succeed");"#.to_string()
    }
    Expect::Ok { ok } => format!(
      r##"{NORMALIZE}
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text.clone()));
    let expected: serde_json::Value = serde_json::from_str(r#"{expected}"#).unwrap();
    assert_eq!(value, expected, "unexpected response: {{text}}");"##,
      expected = serde_json::to_string(ok).expect("expect.ok serialize"),
    ),
    Expect::OkContains { ok_contains } => format!(
      "{NORMALIZE}\n    assert!(text.contains({needle}), \"unexpected response: {{text}}\");",
      needle = json_string(ok_contains),
    ),
    Expect::ErrContains { err_contains } => format!(
      r#"    let err = res.expect_err("expected the command to fail");
    let msg = err.to_string();
    assert!(msg.contains({needle}), "unexpected error: {{msg}}");"#,
      needle = json_string(err_contains),
    ),
  }
}

pub fn sanitize(s: &str) -> String {
  s.chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect()
}

fn indent(text: &str, n: usize) -> String {
  let pad = " ".repeat(n);
  text
    .split('\n')
    .map(|l| {
      if l.trim().is_empty() {
        l.to_string()
      } else {
        format!("{pad}{l}")
      }
    })
    .collect::<Vec<_>>()
    .join("\n")
    + "\n"
}

// JSON.stringify-compatible string quoting for embedding literals in generated code
fn json_string(s: &str) -> String {
  serde_json::to_string(s).expect("string serialize")
}

#[cfg(test)]
mod tests {
  use super::{apply_harness, cargo_toml_with_harness, generate_ipc_test_file, lib_name, IpcCase};
  use crate::tutorial::{Assertion, AssertionKind, Expect, ExpectKeyword};

  const SCAFFOLD: &str = "[package]\nname = \"tatu-app\"\n\n[dependencies]\ntauri = { version = \"2\", features = [] }\n";

  // the whole point of the seam: the generated app is the tutorial's own crate
  #[test]
  fn the_generated_app_is_built_from_the_tutorial_crate() {
    let denied = Assertion {
      kind: AssertionKind::IpcAcl,
      command: Some("plugin:fs|read_text_file".into()),
      args: None,
      expect: Some(Expect::Keyword(ExpectKeyword::Denied)),
      setup: None,
      run: None,
    };
    let out = generate_ipc_test_file(
      "tatu_app_lib",
      &[IpcCase {
        label: "step_0_read".into(),
        assertion: &denied,
      }],
    );
    assert!(
      out.contains("tatu_app_lib::configure(mock_builder())"),
      "{out}"
    );
    assert!(
      out.contains("Permissions associated with this command"),
      "{out}"
    );
    assert!(out.contains("not an ACL denial"), "{out}");
  }

  #[test]
  fn lib_name_prefers_the_lib_table() {
    assert_eq!(
      lib_name(&format!("{SCAFFOLD}\n[lib]\nname = \"tatu_app_lib\"\n")).unwrap(),
      "tatu_app_lib"
    );
  }

  // cargo's own fallback: the package name with `-` → `_`
  #[test]
  fn lib_name_falls_back_to_the_package_name() {
    assert_eq!(lib_name(SCAFFOLD).unwrap(), "tatu_app");
  }

  // the harness is a revertible layer; anything it leaves behind lands in the
  // next step's snapshots as runner scaffolding readers never wrote
  #[test]
  fn apply_then_restore_leaves_the_work_tree_untouched() {
    let dir = std::env::temp_dir().join(format!("tatu-harness-roundtrip-{}", std::process::id()));
    let src_tauri = dir.join("src-tauri");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&src_tauri).expect("temp dir");
    let cargo_toml = format!("{SCAFFOLD}\n[lib]\nname = \"tatu_app_lib\"\n");
    let build_rs = "fn main() {\n    tauri_build::build()\n}\n";
    std::fs::write(src_tauri.join("Cargo.toml"), &cargo_toml).expect("write Cargo.toml");
    std::fs::write(src_tauri.join("build.rs"), build_rs).expect("write build.rs");

    let guard = apply_harness(&dir).expect("applies");
    assert_eq!(guard.lib_name, "tatu_app_lib");
    guard.restore(None).expect("restores");

    assert_eq!(
      std::fs::read_to_string(src_tauri.join("Cargo.toml")).unwrap(),
      cargo_toml
    );
    assert_eq!(
      std::fs::read_to_string(src_tauri.join("build.rs")).unwrap(),
      build_rs
    );
    assert!(!src_tauri.join("tatu-test-manifest.xml").exists());
    let _ = std::fs::remove_dir_all(&dir);
  }

  fn tauri_dev_dep(toml: &str) -> toml_edit::Item {
    let doc: toml_edit::DocumentMut = toml.parse().expect("output is valid TOML");
    doc["dev-dependencies"]["tauri"].clone()
  }

  #[test]
  fn creates_dev_dependencies_when_absent() {
    let out = cargo_toml_with_harness(SCAFFOLD).unwrap();
    let dep = tauri_dev_dep(&out);
    assert_eq!(dep["version"].as_str(), Some("2"));
    assert_eq!(dep["features"][0].as_str(), Some("test"));
    assert!(out.contains("tatu:harness"));
  }

  #[test]
  fn merges_into_existing_dev_dependencies() {
    let toml = format!("{SCAFFOLD}\n[dev-dependencies]\nserde_json = \"1\"\n");
    let out = cargo_toml_with_harness(&toml).unwrap();
    assert_eq!(out.matches("[dev-dependencies]").count(), 1);
    let doc: toml_edit::DocumentMut = out.parse().unwrap();
    assert!(doc["dev-dependencies"]["serde_json"].is_str());
    assert_eq!(
      doc["dev-dependencies"]["tauri"]["features"][0].as_str(),
      Some("test")
    );
  }

  #[test]
  fn widens_a_plain_version_tauri_dev_dep() {
    let toml = format!("{SCAFFOLD}\n[dev-dependencies]\ntauri = \"2.5\"\n");
    let dep = tauri_dev_dep(&cargo_toml_with_harness(&toml).unwrap());
    assert_eq!(dep["version"].as_str(), Some("2.5"));
    assert_eq!(dep["features"][0].as_str(), Some("test"));
  }

  #[test]
  fn adds_test_to_existing_features() {
    let toml = format!(
      "{SCAFFOLD}\n[dev-dependencies]\ntauri = {{ version = \"2\", features = [\"tracing\"] }}\n"
    );
    let dep = tauri_dev_dep(&cargo_toml_with_harness(&toml).unwrap());
    let features: Vec<_> = dep["features"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|v| v.as_str())
      .collect();
    assert_eq!(features, ["tracing", "test"]);
  }

  #[test]
  fn reapplying_is_idempotent() {
    let once = cargo_toml_with_harness(SCAFFOLD).unwrap();
    assert_eq!(cargo_toml_with_harness(&once).unwrap(), once);
  }

  // the marker must land for every dev-dep shape — without it the re-entry
  // guard and the manifest leak assertion are both blind
  #[test]
  fn dotted_table_dev_dep_still_gets_the_marker() {
    let toml = format!("{SCAFFOLD}\n[dev-dependencies.tauri]\nversion = \"2\"\n");
    let out = cargo_toml_with_harness(&toml).unwrap();
    assert!(out.contains("tatu:harness"), "marker missing: {out}");
    let dep = tauri_dev_dep(&out);
    assert_eq!(dep["version"].as_str(), Some("2"));
    assert_eq!(dep["features"][0].as_str(), Some("test"));
  }
}
