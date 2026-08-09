use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::helpers;

// pool conventions: every base is scaffolded with the same app name and
// identifier so tutorials can share it
const APP_NAME: &str = "tatu-app";
const IDENTIFIER: &str = "com.tatu.app";
const MANAGER: &str = "pnpm";

// files create-tauri-app emits that the vendored bases deliberately drop: the
// root .gitignore would hide base files from git, and the README is CTA's own
const DROPPED: [&str; 2] = [".gitignore", "README.md"];

pub fn revendor(target: &Path) -> Result<()> {
  let target = std::path::absolute(target)?;
  let (template, version) = parse_base_name(&target)?;

  // require the exact CTA version so the dir name never lies about its content
  let installed = cta_version()?;
  if installed != version {
    return Err(Error::Runner(format!(
      "installed create-tauri-app is {installed} but the target dir names {version} — run `cargo install create-tauri-app@{version} --locked` first"
    )));
  }

  let temp = std::env::temp_dir().join(format!("tatu-revendor-{template}-{version}"));
  if temp.exists() {
    fs::remove_dir_all(&temp)?;
  }
  fs::create_dir_all(&temp)?;
  run(
    Command::new("cargo")
      .args([
        "create-tauri-app",
        APP_NAME,
        "--template",
        &template,
        "--manager",
        MANAGER,
        "--identifier",
        IDENTIFIER,
        "--yes",
      ])
      .current_dir(&temp),
    "create-tauri-app",
  )?;
  let scaffold = temp.join(APP_NAME);
  for dropped in DROPPED {
    let _ = fs::remove_file(scaffold.join(dropped));
  }
  // CTA emits no lockfile; the vendored base carries one so tutorial builds
  // are reproducible until the next explicit re-vendor
  run(
    Command::new("cargo")
      .args(["generate-lockfile"])
      .current_dir(scaffold.join("src-tauri")),
    "cargo generate-lockfile",
  )?;

  let previous = snapshot(&target)?;
  let fresh = snapshot(&scaffold)?;
  if target.exists() {
    fs::remove_dir_all(&target)?;
  }
  helpers::copy_dir(&scaffold, &target)?;
  report(&previous, &fresh);
  println!("re-vendored {}", target.display());
  println!(
    "next: `tatu check` every tutorial using this base (the re-vendor guard reports overlay conflicts), review, then `tatu bless`"
  );
  Ok(())
}

// pool dir names are <template>@<cta-version>
fn parse_base_name(target: &Path) -> Result<(String, String)> {
  let name = target
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or_default();
  match name.split_once('@') {
    Some((template, version)) if !template.is_empty() && !version.is_empty() => {
      Ok((template.to_string(), version.to_string()))
    }
    _ => Err(Error::Runner(format!(
      "base dir name must be <template>@<cta-version>, got \"{name}\""
    ))),
  }
}

fn cta_version() -> Result<String> {
  let output = Command::new("cargo")
    .args(["create-tauri-app", "--version"])
    .output()
    .map_err(|e| {
      Error::Runner(format!(
        "cannot run `cargo create-tauri-app` ({e}) — install it with `cargo install create-tauri-app --locked`"
      ))
    })?;
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run(command: &mut Command, label: &str) -> Result<()> {
  let output = command.output()?;
  if !output.status.success() {
    return Err(Error::Runner(format!(
      "{label} failed:\n{}\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    )));
  }
  Ok(())
}

fn snapshot(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
  let mut map = BTreeMap::new();
  if !dir.exists() {
    return Ok(map);
  }
  for entry in walkdir::WalkDir::new(dir) {
    let entry = entry.map_err(|e| Error::Runner(e.to_string()))?;
    if !entry.file_type().is_file() {
      continue;
    }
    map.insert(
      helpers::rel_slash(dir, entry.path()),
      fs::read(entry.path())?,
    );
  }
  Ok(map)
}

fn report(previous: &BTreeMap<String, Vec<u8>>, fresh: &BTreeMap<String, Vec<u8>>) {
  if previous.is_empty() {
    println!("new base: {} files", fresh.len());
    return;
  }
  let mut unchanged = 0;
  for (file, bytes) in fresh {
    match previous.get(file) {
      None => println!("  added     {file}"),
      Some(old) if old != bytes => println!("  changed   {file}"),
      Some(_) => unchanged += 1,
    }
  }
  for file in previous.keys() {
    if !fresh.contains_key(file) {
      println!("  removed   {file}");
    }
  }
  println!("  unchanged {unchanged} file(s)");
}

#[cfg(test)]
mod tests {
  use super::parse_base_name;
  use std::path::Path;

  #[test]
  fn base_name_parses_template_and_version() {
    let (t, v) = parse_base_name(Path::new("bases/vanilla-ts@4.7.3")).unwrap();
    assert_eq!(t, "vanilla-ts");
    assert_eq!(v, "4.7.3");
  }

  #[test]
  fn base_name_without_version_is_rejected() {
    assert!(parse_base_name(Path::new("bases/vanilla-ts")).is_err());
    assert!(parse_base_name(Path::new("bases/@4.7.3")).is_err());
  }
}
