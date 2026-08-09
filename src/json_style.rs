use serde_json::Value;

use crate::error::{Error, Result};

// Style-preserving rendering for json-merge mutations: the recorded diff must
// show the merge and nothing else, and the target files mix styles no
// formatter reproduces (create-tauri-app's capability file writes
// `"windows": ["main"]` inline but `"permissions"` multi-line). So subtrees
// the merge left untouched are spliced verbatim from the original text, and
// only changed nodes are re-rendered.
pub fn render_merged(original_text: &str, merged: &Value) -> Result<String> {
  let original: Value = serde_json::from_str(original_text)?;
  let spanned = parse_spanned(original_text)?;
  let indent = detect_indent(original_text);
  let mut out = String::new();
  write_node(
    &mut out,
    merged,
    Some((&original, &spanned)),
    original_text,
    &indent,
    0,
  );
  if original_text.ends_with('\n') {
    out.push('\n');
  }
  // a writer bug must fail the mutation loudly, never record corrupted content
  let reparsed: Value = serde_json::from_str(&out).map_err(|e| {
    Error::Runner(format!(
      "internal: style-preserving render produced invalid JSON ({e}) — this is a tatu bug"
    ))
  })?;
  if &reparsed != merged {
    return Err(Error::Runner(
      "internal: style-preserving render changed the merged value — this is a tatu bug".into(),
    ));
  }
  Ok(out)
}

fn write_node(
  out: &mut String,
  node: &Value,
  original: Option<(&Value, &Spanned)>,
  text: &str,
  indent: &str,
  depth: usize,
) {
  if let Some((original_value, spanned)) = original {
    if original_value == node {
      out.push_str(&text[spanned.start..spanned.end]);
      return;
    }
  }
  match node {
    Value::Object(map) if map.is_empty() => out.push_str("{}"),
    Value::Object(map) => {
      out.push_str("{\n");
      let pad = indent.repeat(depth + 1);
      for (i, (key, value)) in map.iter().enumerate() {
        out.push_str(&pad);
        out.push_str(&serde_json::to_string(key).expect("string serialize"));
        out.push_str(": ");
        write_node(
          out,
          value,
          child_by_key(original, key),
          text,
          indent,
          depth + 1,
        );
        if i + 1 < map.len() {
          out.push(',');
        }
        out.push('\n');
      }
      out.push_str(&indent.repeat(depth));
      out.push('}');
    }
    Value::Array(items) if items.is_empty() => out.push_str("[]"),
    Value::Array(items) => {
      out.push_str("[\n");
      let pad = indent.repeat(depth + 1);
      for (i, value) in items.iter().enumerate() {
        out.push_str(&pad);
        write_node(
          out,
          value,
          child_by_index(original, i),
          text,
          indent,
          depth + 1,
        );
        if i + 1 < items.len() {
          out.push(',');
        }
        out.push('\n');
      }
      out.push_str(&indent.repeat(depth));
      out.push(']');
    }
    scalar => out.push_str(&serde_json::to_string(scalar).expect("scalar serialize")),
  }
}

fn child_by_key<'a>(
  original: Option<(&'a Value, &'a Spanned)>,
  key: &str,
) -> Option<(&'a Value, &'a Spanned)> {
  let (value, spanned) = original?;
  let SpannedKind::Object(entries) = &spanned.kind else {
    return None;
  };
  let child_span = &entries.iter().find(|(k, _)| k == key)?.1;
  Some((value.as_object()?.get(key)?, child_span))
}

fn child_by_index<'a>(
  original: Option<(&'a Value, &'a Spanned)>,
  index: usize,
) -> Option<(&'a Value, &'a Spanned)> {
  let (value, spanned) = original?;
  let SpannedKind::Array(items) = &spanned.kind else {
    return None;
  };
  Some((value.as_array()?.get(index)?, items.get(index)?))
}

// the smallest indent of any indented line — the first indented line alone
// can sit deeper than depth 1 (e.g. a key on the opening-brace line)
fn detect_indent(text: &str) -> String {
  text
    .lines()
    .filter_map(|line| {
      let ws: String = line.chars().take_while(|c| c.is_whitespace()).collect();
      (!ws.is_empty() && ws.len() < line.len()).then_some(ws)
    })
    .min_by_key(String::len)
    .unwrap_or_else(|| "  ".to_string())
}

// Minimal span-tracking JSON parse: byte offsets of every node in the source,
// so unchanged subtrees can be copied out verbatim. Correctness of the output
// does not rest on this parser — render_merged re-parses and compares.
struct Spanned {
  start: usize,
  end: usize,
  kind: SpannedKind,
}

enum SpannedKind {
  Object(Vec<(String, Spanned)>),
  Array(Vec<Spanned>),
  Scalar,
}

fn parse_spanned(text: &str) -> Result<Spanned> {
  let mut parser = Parser {
    bytes: text.as_bytes(),
    pos: 0,
  };
  parser.skip_ws();
  parser.value()
}

struct Parser<'a> {
  bytes: &'a [u8],
  pos: usize,
}

impl Parser<'_> {
  fn skip_ws(&mut self) {
    while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
      self.pos += 1;
    }
  }

  fn expect(&mut self, byte: u8) -> Result<()> {
    if self.bytes.get(self.pos) == Some(&byte) {
      self.pos += 1;
      Ok(())
    } else {
      Err(Error::Runner(format!(
        "json-merge target: expected {:?} at byte {}",
        byte as char, self.pos
      )))
    }
  }

  fn value(&mut self) -> Result<Spanned> {
    let start = self.pos;
    let kind = match self.bytes.get(self.pos) {
      Some(b'{') => {
        self.pos += 1;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b'}') {
          self.pos += 1;
        } else {
          loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            entries.push((key, self.value()?));
            self.skip_ws();
            if self.bytes.get(self.pos) == Some(&b',') {
              self.pos += 1;
            } else {
              self.expect(b'}')?;
              break;
            }
          }
        }
        SpannedKind::Object(entries)
      }
      Some(b'[') => {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&b']') {
          self.pos += 1;
        } else {
          loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            if self.bytes.get(self.pos) == Some(&b',') {
              self.pos += 1;
            } else {
              self.expect(b']')?;
              break;
            }
          }
        }
        SpannedKind::Array(items)
      }
      Some(b'"') => {
        self.string()?;
        SpannedKind::Scalar
      }
      Some(_) => {
        // number / true / false / null — runs to the next delimiter
        while !matches!(
          self.bytes.get(self.pos),
          None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
        ) {
          self.pos += 1;
        }
        SpannedKind::Scalar
      }
      None => {
        return Err(Error::Runner(
          "json-merge target: unexpected end of input".into(),
        ));
      }
    };
    Ok(Spanned {
      start,
      end: self.pos,
      kind,
    })
  }

  /// consumes a string literal, returning its decoded value
  fn string(&mut self) -> Result<String> {
    let start = self.pos;
    self.expect(b'"')?;
    loop {
      match self.bytes.get(self.pos) {
        Some(b'\\') => self.pos += 2,
        Some(b'"') => {
          self.pos += 1;
          break;
        }
        Some(_) => self.pos += 1,
        None => {
          return Err(Error::Runner(
            "json-merge target: unterminated string".into(),
          ));
        }
      }
    }
    let raw = std::str::from_utf8(&self.bytes[start..self.pos])
      .map_err(|e| Error::Runner(format!("json-merge target: {e}")))?;
    serde_json::from_str(raw).map_err(|e| Error::Runner(format!("json-merge target: {e}")))
  }
}

#[cfg(test)]
mod tests {
  use super::{detect_indent, render_merged};
  use serde_json::json;

  // create-tauri-app's real capability style: one array inline, one multi-line
  const CAPABILITY: &str = r#"{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "opener:default"
  ]
}
"#;

  #[test]
  fn unchanged_document_renders_verbatim() {
    let merged = serde_json::from_str(CAPABILITY).unwrap();
    assert_eq!(render_merged(CAPABILITY, &merged).unwrap(), CAPABILITY);
  }

  #[test]
  fn untouched_inline_array_stays_inline() {
    let mut merged: serde_json::Value = serde_json::from_str(CAPABILITY).unwrap();
    merged["permissions"]
      .as_array_mut()
      .unwrap()
      .push(json!("fs:default"));
    let out = render_merged(CAPABILITY, &merged).unwrap();
    assert!(out.contains("\"windows\": [\"main\"],"), "reflowed: {out}");
    assert!(
      out.contains("    \"opener:default\",\n    \"fs:default\"\n  ]"),
      "unexpected permissions layout: {out}"
    );
  }

  #[test]
  fn changed_scalar_keeps_sibling_layout() {
    let original = "{\n  \"build\": {\n    \"devUrl\": \"http://localhost:1420\",\n    \"frontendDist\": \"../dist\"\n  },\n  \"bundle\": {\n    \"icon\": [\n      \"a.png\",\n      \"b.png\"\n    ]\n  }\n}\n";
    let mut merged: serde_json::Value = serde_json::from_str(original).unwrap();
    merged["build"]["devUrl"] = json!("http://localhost:3000");
    let out = render_merged(original, &merged).unwrap();
    assert_eq!(out, original.replace("1420", "3000"));
  }

  #[test]
  fn strings_with_brackets_and_escapes_parse() {
    let original = "{\n  \"a\": \"}] \\\" \\\\ tricky\",\n  \"b\": 1\n}";
    let mut merged: serde_json::Value = serde_json::from_str(original).unwrap();
    merged["b"] = json!(2);
    let out = render_merged(original, &merged).unwrap();
    assert!(out.contains("\"a\": \"}] \\\" \\\\ tricky\""));
    assert!(out.contains("\"b\": 2"));
    assert!(!out.ends_with('\n'), "trailing newline invented");
  }

  #[test]
  fn new_keys_render_with_detected_indent() {
    let original = "{\n    \"a\": 1\n}\n";
    let mut merged: serde_json::Value = serde_json::from_str(original).unwrap();
    merged["b"] = json!({ "c": [] });
    let out = render_merged(original, &merged).unwrap();
    assert_eq!(
      out,
      "{\n    \"a\": 1,\n    \"b\": {\n        \"c\": []\n    }\n}\n"
    );
  }

  #[test]
  fn indent_detection_ignores_a_deep_first_line() {
    assert_eq!(
      detect_indent("{ \"a\": {\n    \"b\": 1,\n  \"c\": 2\n}}"),
      "  "
    );
    assert_eq!(detect_indent("{\n\t\"a\": 1\n}"), "\t");
  }
}
