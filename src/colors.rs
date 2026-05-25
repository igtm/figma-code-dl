//! Substitute bare hex colors in the TSX output with `var(--css-name, #hex)`
//! references, driven by a JSON file that was previously dumped from Figma
//! Variables (`figma-code-dl --dump-variables .figma/variables.json`).
//!
//! Only color variables that have a `codeSyntax.WEB` value set on the Figma
//! side get used for substitution. Variables without a `codeSyntax.WEB` are
//! still saved in the JSON for reference but are skipped here.
//!
//! Substitution example, with `#DEF4F2 -> --blue-100` in the file:
//!
//!   `bg-[#def4f2]` → `bg-[var(--blue-100,#def4f2)]`
//!
//! Only the Tailwind arbitrary-value form `[#XXX(XXX)]` is targeted, since
//! that's what `get_design_context` emits for unbound fills/strokes.

use anyhow::{Context, Result};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// The on-disk file `.figma/variables.json`.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct VariableFile {
    #[serde(rename = "$comment", default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub variables: Vec<VariableEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VariableEntry {
    /// `codeSyntax.WEB` from Figma. `None` when the variable doesn't have a
    /// WEB code-syntax set; in that case substitution skips it.
    #[serde(default)]
    pub css: Option<String>,
    /// Figma's full variable name (e.g. `color/semantic/background/green`).
    /// Kept for reference / human navigation; not used for substitution.
    #[serde(default)]
    pub figma_name: Option<String>,
    /// Resolved color as `#RRGGBB` (uppercase, 6-digit, no alpha).
    pub hex: String,
}

/// In-memory lookup table built from the file. Keys are normalized
/// (uppercase, 6-digit) hex strings without the leading `#`.
#[derive(Debug, Default, Clone)]
pub struct ColorMap {
    hex_to_css: HashMap<String, String>,
}

impl ColorMap {
    pub fn from_file(file: &VariableFile) -> Self {
        let mut hex_to_css = HashMap::new();
        // Iterate in insertion order so the first entry per hex wins.
        for v in &file.variables {
            let Some(css) = v.css.as_deref() else {
                continue;
            };
            if css.is_empty() {
                continue;
            }
            let Some(key) = normalize_hex(&v.hex) else {
                continue;
            };
            hex_to_css.entry(key).or_insert_with(|| css.to_string());
        }
        Self { hex_to_css }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read_to_string(path)
            .with_context(|| format!("reading colors file {}", path.display()))?;
        let file: VariableFile = serde_json::from_str(&bytes)
            .with_context(|| format!("parsing JSON {}", path.display()))?;
        Ok(Self::from_file(&file))
    }

    pub fn len(&self) -> usize {
        self.hex_to_css.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hex_to_css.is_empty()
    }

    pub fn lookup(&self, hex: &str) -> Option<&str> {
        normalize_hex(hex).and_then(|k| self.hex_to_css.get(&k).map(String::as_str))
    }
}

#[derive(Debug, Default)]
pub struct ColorReport {
    pub substitutions: u32,
    /// hex (uppercase, 6-digit) → count of substitutions performed
    pub by_hex: HashMap<String, u32>,
}

/// Run the substitution pass.
pub fn process(src: &str, map: &ColorMap) -> (String, ColorReport) {
    let mut report = ColorReport::default();
    if map.is_empty() {
        return (src.to_string(), report);
    }

    let re = hex_in_brackets_re();
    let out = re.replace_all(src, |caps: &Captures| {
        let hex = caps.get(1).unwrap().as_str();
        match map.lookup(hex) {
            Some(css) => {
                report.substitutions += 1;
                let key = normalize_hex(hex).unwrap_or_else(|| hex.to_ascii_uppercase());
                *report.by_hex.entry(key).or_default() += 1;
                // Match MCP's existing escape style: `/` in CSS variable names
                // is emitted as `\/` so the resulting source looks identical
                // to what `get_design_context` produces for variable-bound
                // layers (e.g. `--color\/semantic\/background\/green`).
                let css_escaped = css.replace('/', "\\/");
                format!("[var({},#{})]", css_escaped, hex)
            }
            None => caps.get(0).unwrap().as_str().to_string(),
        }
    });
    (out.into_owned(), report)
}

/// Match `[#XXX]` or `[#XXXXXX]` (3 or 6 hex digits). The leading `[` and
/// trailing `]` are kept out of group 1 so the value can be looked up
/// directly. The `#` is also outside group 1.
fn hex_in_brackets_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[#([0-9a-fA-F]{6}|[0-9a-fA-F]{3})\]").unwrap())
}

/// Normalize `#abc` / `#aabbcc` / `aabbcc` / `abc` to `AABBCC` (uppercase,
/// 6-digit, no leading `#`).
fn normalize_hex(s: &str) -> Option<String> {
    let s = s.trim_start_matches('#');
    let chars: Vec<char> = s.chars().collect();
    let expanded: String = match chars.len() {
        3 if chars.iter().all(|c| c.is_ascii_hexdigit()) => chars
            .iter()
            .flat_map(|c| std::iter::repeat(*c).take(2))
            .collect(),
        6 if chars.iter().all(|c| c.is_ascii_hexdigit()) => s.to_string(),
        _ => return None,
    };
    Some(expanded.to_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn file_from(pairs: &[(Option<&str>, &str)]) -> VariableFile {
        VariableFile {
            comment: None,
            generated_at: None,
            variables: pairs
                .iter()
                .map(|(css, hex)| VariableEntry {
                    css: css.map(|s| s.to_string()),
                    figma_name: None,
                    hex: hex.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn substitutes_exact_match() {
        // No `/` in the css name → escape is a no-op, output identical to user's
        // preferred `--blue-100` form.
        let file = file_from(&[(Some("--blue-100"), "#DEF4F2")]);
        let map = ColorMap::from_file(&file);
        let src = r#"<div className="bg-[#def4f2] text-[#3D4047]">x</div>"#;
        let (out, r) = process(src, &map);
        assert_eq!(
            out,
            r#"<div className="bg-[var(--blue-100,#def4f2)] text-[#3D4047]">x</div>"#
        );
        assert_eq!(r.substitutions, 1);
    }

    #[test]
    fn case_insensitive_lookup() {
        let file = file_from(&[(Some("--c"), "#def4f2")]);
        let map = ColorMap::from_file(&file);
        let (out, _) = process(r#"className="bg-[#DEF4F2]""#, &map);
        assert!(out.contains("var(--c,#DEF4F2)"));
    }

    #[test]
    fn three_digit_hex_normalized() {
        // The dumped Figma value is full hex; the source may use the 3-digit form.
        let file = file_from(&[(Some("--white"), "#FFFFFF")]);
        let map = ColorMap::from_file(&file);
        let (out, r) = process(r#"className="bg-[#fff]""#, &map);
        assert_eq!(out, r#"className="bg-[var(--white,#fff)]""#);
        assert_eq!(r.substitutions, 1);
    }

    #[test]
    fn variable_without_codesyntax_skipped() {
        let file = file_from(&[(None, "#DEF4F2"), (Some(""), "#CFCFD1")]);
        let map = ColorMap::from_file(&file);
        assert!(map.is_empty());
        let (out, r) = process(r#"className="bg-[#def4f2] border-[#cfcfd1]""#, &map);
        assert_eq!(out, r#"className="bg-[#def4f2] border-[#cfcfd1]""#);
        assert_eq!(r.substitutions, 0);
    }

    #[test]
    fn unmatched_hex_left_alone() {
        let file = file_from(&[(Some("--known"), "#DEF4F2")]);
        let map = ColorMap::from_file(&file);
        let (out, r) = process(r#"className="bg-[#abcdef]""#, &map);
        assert_eq!(out, r#"className="bg-[#abcdef]""#);
        assert_eq!(r.substitutions, 0);
    }

    #[test]
    fn multiple_substitutions_same_hex() {
        let file = file_from(&[(Some("--g"), "#DEF4F2")]);
        let map = ColorMap::from_file(&file);
        let (out, r) = process(
            r#"<div className="bg-[#def4f2]"><span className="text-[#def4f2]" /></div>"#,
            &map,
        );
        assert_eq!(r.substitutions, 2);
        assert_eq!(r.by_hex.get("DEF4F2"), Some(&2));
        assert_eq!(out.matches("var(--g,").count(), 2);
    }

    #[test]
    fn first_entry_wins_on_hex_collision() {
        let file = file_from(&[
            (Some("--primary"), "#CFCFD1"),
            (Some("--secondary"), "#CFCFD1"),
        ]);
        let map = ColorMap::from_file(&file);
        let (out, _) = process(r#"className="border-[#cfcfd1]""#, &map);
        assert!(out.contains("--primary"));
        assert!(!out.contains("--secondary"));
    }

    #[test]
    fn escapes_slashes_to_match_mcp_format() {
        let file = file_from(&[(Some("--color/semantic/background/green"), "#DEF4F2")]);
        let map = ColorMap::from_file(&file);
        let src = r#"className="bg-[#def4f2]""#;
        let (out, _) = process(src, &map);
        assert_eq!(
            out,
            r#"className="bg-[var(--color\/semantic\/background\/green,#def4f2)]""#
        );
    }

    #[test]
    fn does_not_touch_existing_var_form() {
        let file = file_from(&[(Some("--blue-100"), "#DEF4F2")]);
        let map = ColorMap::from_file(&file);
        // MCP already emitted var(...) form. Our regex requires `[#...]` shape,
        // so this should not be re-wrapped.
        let src = r#"className="bg-[var(--color\/semantic\/background\/green,#def4f2)]""#;
        let (out, r) = process(src, &map);
        assert_eq!(out, src);
        assert_eq!(r.substitutions, 0);
    }

    #[test]
    fn roundtrip_json() {
        let file = VariableFile {
            comment: Some("hello".into()),
            generated_at: Some("2026-05-25T00:00:00Z".into()),
            variables: vec![
                VariableEntry {
                    css: Some("--blue-100".into()),
                    figma_name: Some("color/semantic/background/green".into()),
                    hex: "#DEF4F2".into(),
                },
                VariableEntry {
                    css: None,
                    figma_name: Some("black/black_60".into()),
                    hex: "#6E7075".into(),
                },
            ],
        };
        let s = serde_json::to_string_pretty(&file).unwrap();
        let parsed: VariableFile = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.variables.len(), 2);
        assert_eq!(parsed.variables[0].css.as_deref(), Some("--blue-100"));
        assert!(parsed.variables[1].css.is_none());
    }
}
