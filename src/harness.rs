use std::fs;
use std::path::Path;

use crate::error::Result;
use crate::tutorial::{Assertion, Expect, ExpectKeyword, Harness};

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

const DEV_DEP_BLOCK: &str = "
# tatu:harness — tauri::test is feature-gated
[dev-dependencies]
tauri = { version = \"2\", features = [\"test\"] }
";

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
}

pub fn apply_harness(work_dir: &Path) -> Result<HarnessGuard> {
  let src_tauri = work_dir.join("src-tauri");

  let cargo_toml = src_tauri.join("Cargo.toml");
  let cargo_toml_original = fs::read_to_string(&cargo_toml)?;
  if !cargo_toml_original.contains("tatu:harness") {
    fs::write(
      &cargo_toml,
      format!("{}\n{}", cargo_toml_original.trim_end(), DEV_DEP_BLOCK),
    )?;
  }

  let build_rs = src_tauri.join("build.rs");
  let build_rs_original = fs::read_to_string(&build_rs)?;
  if !build_rs_original.contains("tatu:harness") {
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
// host one Tauri app, so each phase gets its own test binary.
// `harness.prelude` in tutorial.yaml carries verbatim
// Rust (e.g. a copy of the app command under test: #[tauri::command] handlers
// aren't importable across crates).
pub fn generate_ipc_test_file(harness: Option<&Harness>, cases: &[IpcCase<'_>]) -> String {
  let empty = Harness {
    prelude: None,
    handlers: None,
    plugins: None,
  };
  let harness = harness.unwrap_or(&empty);

  let plugin_lines = harness
    .plugins
    .as_deref()
    .unwrap_or_default()
    .iter()
    .map(|p| format!("        .plugin({p}::init())"))
    .collect::<Vec<_>>()
    .join("\n");
  let handler_list = harness.handlers.as_deref().unwrap_or_default().join(", ");

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
        r#"#[test]
fn {fn_name}() {{
    let app = build_app();
{setup}    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let res = get_ipc_response(
        &webview,
        InvokeRequest {{
            cmd: {command}.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // the webview's own URL (devUrl under `cargo test`) — the one origin
            // that is Origin::Local on every platform; a hardcoded
            // http://tauri.localhost is remote (= fully ACL-denied) off Windows
            url: webview.url().unwrap(),
            body: InvokeBody::Json(serde_json::from_str(r#{args_open}{args}{args_close}#).unwrap()),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }},
    );
{expect}
}}"#,
        args_open = '"',
        args = serde_json::to_string(&args).expect("args serialize"),
        args_close = '"',
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

  format!(
    "// generated by tatu — do not edit; regenerated on every run
#![allow(unused_imports, dead_code)]
use tauri::ipc::{{CallbackFn, InvokeBody}};
use tauri::test::{{get_ipc_response, mock_builder, INVOKE_KEY}};
use tauri::webview::InvokeRequest;
use tauri::Manager;

{prelude}

fn build_app() -> tauri::App<tauri::test::MockRuntime> {{
    mock_builder()
{plugin_lines}
        .invoke_handler(tauri::generate_handler![{handler_list}])
        .build(tauri::generate_context!())
        .expect(\"failed to build app from real context\")
}}

{tests}
",
    prelude = harness.prelude.as_deref().unwrap_or_default(),
  )
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
    Expect::Keyword(ExpectKeyword::Denied) => {
      r#"    let err = res.expect_err("expected an ACL denial");
    let msg = err.to_string();
    assert!(msg.contains("not allowed"), "expected ACL denial, got: {msg}");"#
        .to_string()
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
