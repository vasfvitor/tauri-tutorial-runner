use std::fs;
use std::path::Path;

use crate::error::Result;

pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
  for entry in walkdir::WalkDir::new(from) {
    let entry = entry.map_err(|e| crate::error::Error::Runner(e.to_string()))?;
    let rel = entry
      .path()
      .strip_prefix(from)
      .expect("walkdir yields children of its root");
    let dest = to.join(rel);
    if entry.file_type().is_dir() {
      fs::create_dir_all(&dest)?;
    } else {
      if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
      }
      fs::copy(entry.path(), &dest)?;
    }
  }
  Ok(())
}

pub fn last_lines(text: &str, n: usize, prefix: &str) -> String {
  let lines: Vec<&str> = text.split('\n').collect();
  let start = lines.len().saturating_sub(n);
  lines[start..]
    .iter()
    .map(|l| format!("{prefix}{l}"))
    .collect::<Vec<_>>()
    .join("\n")
}
