//! Fetch Figma color Variables via the MCP `get_variable_defs` tool and write
//! them to `.figma/variables.json` in the format consumed by `colors::ColorMap`.
//!
//! The exact shape returned by `get_variable_defs` depends on the Figma MCP
//! server version. We parse defensively:
//!
//!   - Each `ContentBlock.text` is tried as JSON.
//!   - The top-level value can be an array of variables, a `{ "variables": [...] }`
//!     object, or a `{ "collections": [...] }` object — all three are handled.
//!   - For each variable, we look for `name` (string) and either
//!     `codeSyntax.WEB` (Figma Plugin API form) or a flat `codeSyntaxWeb` /
//!     `code_syntax_web` (server-flattened form).
//!   - The hex value is read from either `value` (string `#RRGGBB`) or a
//!     `{ r, g, b }` object (each component 0..1 float), or a single-entry
//!     `valuesByMode` object.
//!
//! The raw response is also saved alongside the parsed JSON at
//! `<out_path>.raw.json` so the user can inspect it if our parser missed
//! something.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::colors::{VariableEntry, VariableFile};
use crate::extract::ContentBlock;
use crate::mcp::McpClient;

#[derive(Debug, Default)]
pub struct DumpReport {
    pub variables_total: u32,
    pub variables_with_codesyntax: u32,
    pub raw_path: PathBuf,
}

/// Call `get_variable_defs` for the given node, parse, and write the file.
pub async fn dump(client: &McpClient, node_id: &str, out_path: &Path) -> Result<DumpReport> {
    let blocks = client
        .call_tool(
            "get_variable_defs",
            serde_json::json!({
                "nodeId": node_id,
                "clientFrameworks": "react",
                "clientLanguages": "typescript",
            }),
        )
        .await
        .context("MCP get_variable_defs")?;

    // Persist the raw response next to the parsed file.
    let raw_path = with_extra_extension(out_path, "raw.json");
    if let Some(parent) = raw_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).ok();
    }
    let raw_json =
        serde_json::to_vec_pretty(&blocks).context("serializing raw variable-defs response")?;
    std::fs::write(&raw_path, &raw_json)
        .with_context(|| format!("writing raw response to {}", raw_path.display()))?;

    // Parse.
    let variables = parse_blocks(&blocks)?;
    let variables_total = variables.len() as u32;
    let variables_with_codesyntax = variables
        .iter()
        .filter(|v| v.css.as_deref().map(|s| !s.is_empty()).unwrap_or(false))
        .count() as u32;

    let file = VariableFile {
        comment: Some(
            "Auto-generated from Figma Variables via `figma-code-dl \
             --dump-variables`. Do not edit by hand. Order matters: when \
             multiple variables share a hex, the first entry wins."
                .into(),
        ),
        generated_at: Some(now_iso()),
        variables,
    };

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_vec_pretty(&file).context("serializing variables file")?;
    std::fs::write(out_path, &json).with_context(|| format!("writing {}", out_path.display()))?;

    Ok(DumpReport {
        variables_total,
        variables_with_codesyntax,
        raw_path,
    })
}

fn with_extra_extension(p: &Path, ext: &str) -> PathBuf {
    // `foo.json` -> `foo.raw.json`. Falls back to `foo.<ext>` when there is
    // no current extension.
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("variables");
    let mut name = String::from(stem);
    name.push('.');
    name.push_str(ext);
    p.with_file_name(name)
}

fn now_iso() -> String {
    // Minimal ISO8601 without depending on a heavy time crate. Good enough
    // for a "generated_at" marker.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn epoch_to_ymd_hms(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    // Days since 1970-01-01.
    let days_total = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400) as u32;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;

    // Civil date from days (Howard Hinnant's algorithm, simplified).
    let z = days_total + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_in = era * 400 + yoe as i64;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y_in + 1 } else { y_in };
    (y as i32, m, d, h, mi, s)
}

fn parse_blocks(blocks: &[ContentBlock]) -> Result<Vec<VariableEntry>> {
    let mut out: Vec<VariableEntry> = Vec::new();
    let mut any_json = false;
    for block in blocks {
        if block.block_type != "text" {
            continue;
        }
        let text = &block.text;
        // Try as JSON.
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        any_json = true;
        collect_variables(&value, &mut out);
    }
    if !any_json {
        return Err(anyhow!(
            "no parseable JSON in MCP `get_variable_defs` response (see *.raw.json)"
        ));
    }
    Ok(out)
}

/// Recursively walk likely shapes of the response. The local Figma MCP's
/// `get_variable_defs` actually returns a flat `name -> value` map where the
/// value is `"#hex"` for color variables or `"Font(...)"` for typography
/// variables; the `codeSyntax.WEB` field is NOT exposed. We also handle a
/// couple of richer shapes for resilience.
fn collect_variables(v: &Value, out: &mut Vec<VariableEntry>) {
    match v {
        Value::Array(items) => {
            for item in items {
                if let Some(entry) = variable_from(item) {
                    out.push(entry);
                } else {
                    collect_variables(item, out);
                }
            }
        }
        Value::Object(map) => {
            // Object that itself looks like a variable definition?
            if let Some(entry) = variable_from(v) {
                out.push(entry);
                return;
            }
            // Common wrappers.
            for k in ["variables", "colors", "tokens", "items"] {
                if let Some(child) = map.get(k) {
                    collect_variables(child, out);
                    return;
                }
            }
            if let Some(child) = map.get("collections") {
                collect_variables(child, out);
                return;
            }
            // Map of `name -> value` (the actual local-MCP shape).
            for (k, val) in map {
                match val {
                    // Non-hex strings (e.g. Font descriptors) are silently dropped.
                    Value::String(s) => {
                        if let Some(hex) = parse_hex_string(s) {
                            out.push(VariableEntry {
                                css: derive_css_from_name(k),
                                figma_name: Some(k.clone()),
                                hex,
                            });
                        }
                    }
                    Value::Object(_) => {
                        if let Some(mut entry) = variable_from(val) {
                            if entry.figma_name.is_none() {
                                entry.figma_name = Some(k.clone());
                            }
                            if entry.css.is_none() {
                                entry.css = derive_css_from_name(k);
                            }
                            out.push(entry);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Best-effort CSS variable name derived from a Figma variable's path,
/// used when `codeSyntax.WEB` is unavailable. Matches what
/// `get_design_context` already emits for layers bound to that variable:
///   `Theme/Background/Primary` → `--theme/background/primary`
/// Spaces are hyphenated so the result is a valid CSS identifier:
///   `My Collection/Surface/Default` → `--my-collection/surface/default`
fn derive_css_from_name(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return None;
    }
    let lower = name.to_lowercase();
    let normalized: String = lower
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    Some(format!("--{}", normalized))
}

/// Try to interpret a JSON object as a single Figma color variable. Returns
/// `None` if it doesn't look like one.
fn variable_from(v: &Value) -> Option<VariableEntry> {
    let map = v.as_object()?;

    let kind = map
        .get("resolvedType")
        .or_else(|| map.get("type"))
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_uppercase());
    // Only color variables, when type is known.
    if let Some(k) = &kind
        && k != "COLOR"
        && k != "FLOAT"
        && k != "STRING"
    {
        return None;
    }
    if let Some(k) = &kind
        && k != "COLOR"
    {
        return None;
    }

    let figma_name = map
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    // codeSyntax.WEB or flat variants.
    let css = map
        .get("codeSyntax")
        .and_then(|cs| cs.as_object())
        .and_then(|cs| cs.get("WEB").or_else(|| cs.get("web")))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| {
            map.get("codeSyntaxWeb")
                .or_else(|| map.get("code_syntax_web"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        });

    let hex = extract_hex(map)?;

    Some(VariableEntry {
        css,
        figma_name,
        hex,
    })
}

fn extract_hex(map: &serde_json::Map<String, Value>) -> Option<String> {
    // Direct string forms.
    for k in ["hex", "value", "resolvedValue"] {
        if let Some(s) = map.get(k).and_then(Value::as_str)
            && let Some(h) = parse_hex_string(s)
        {
            return Some(h);
        }
    }
    // RGB object on the same level.
    if let Some(rgb) = rgb_from(map) {
        return Some(rgb_to_hex(rgb));
    }
    // `value: { r, g, b }`
    if let Some(inner) = map.get("value").and_then(Value::as_object)
        && let Some(rgb) = rgb_from(inner)
    {
        return Some(rgb_to_hex(rgb));
    }
    // `valuesByMode: { "<mode>": <rgb or hex> }` — take first.
    if let Some(by_mode) = map.get("valuesByMode").and_then(Value::as_object) {
        for (_, mv) in by_mode {
            if let Some(s) = mv.as_str()
                && let Some(h) = parse_hex_string(s)
            {
                return Some(h);
            }
            if let Some(inner) = mv.as_object()
                && let Some(rgb) = rgb_from(inner)
            {
                return Some(rgb_to_hex(rgb));
            }
        }
    }
    None
}

fn parse_hex_string(s: &str) -> Option<String> {
    let s = s.trim_start_matches('#');
    let chars: Vec<char> = s.chars().collect();
    let expanded: String = match chars.len() {
        3 if chars.iter().all(|c| c.is_ascii_hexdigit()) => chars
            .iter()
            .flat_map(|c| std::iter::repeat_n(*c, 2))
            .collect(),
        6 if chars.iter().all(|c| c.is_ascii_hexdigit()) => s.to_string(),
        8 if chars.iter().all(|c| c.is_ascii_hexdigit()) => s[..6].to_string(),
        _ => return None,
    };
    Some(format!("#{}", expanded.to_ascii_uppercase()))
}

fn rgb_from(map: &serde_json::Map<String, Value>) -> Option<(f64, f64, f64)> {
    let r = map.get("r").and_then(Value::as_f64)?;
    let g = map.get("g").and_then(Value::as_f64)?;
    let b = map.get("b").and_then(Value::as_f64)?;
    Some((r, g, b))
}

fn rgb_to_hex((r, g, b): (f64, f64, f64)) -> String {
    let to_u8 = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to_u8(r), to_u8(g), to_u8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str) -> ContentBlock {
        ContentBlock {
            block_type: "text".into(),
            text: text.into(),
            data: None,
            mime_type: None,
        }
    }

    #[test]
    fn parses_real_local_mcp_shape() {
        // Actual shape returned by the local Dev Mode MCP: flat
        // name -> value map, hex strings or Font(...) descriptors.
        let blocks = vec![block(
            r##"{
              "Theme/Text/Accent": "#29c1af",
              "Theme/Background/Surface": "#ffffff",
              "My Collection/Background/Gray": "#CFCFD1",
              "Typography/Heading/Bold-18": "Font(family: \"Sans\", size: 18)",
              "Palette/Gray/80": "#3D4047"
            }"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out.len(), 4); // Font entry skipped
        let by_name: std::collections::HashMap<_, _> = out
            .iter()
            .map(|v| (v.figma_name.clone().unwrap(), v))
            .collect();
        assert_eq!(
            by_name["Theme/Text/Accent"].css.as_deref(),
            Some("--theme/text/accent")
        );
        assert_eq!(by_name["Theme/Text/Accent"].hex, "#29C1AF");
        assert_eq!(
            by_name["My Collection/Background/Gray"].css.as_deref(),
            Some("--my-collection/background/gray")
        );
        assert_eq!(by_name["Palette/Gray/80"].hex, "#3D4047");
    }

    #[test]
    fn parses_flat_array_of_variables() {
        let blocks = vec![block(
            r##"[
  { "name": "color/semantic/background/green",
    "resolvedType": "COLOR",
    "value": "#DEF4F2",
    "codeSyntax": { "WEB": "--blue-100" } },
  { "name": "black/black_60",
    "resolvedType": "COLOR",
    "value": "#6E7075" }
]"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].css.as_deref(), Some("--blue-100"));
        assert_eq!(out[0].hex, "#DEF4F2");
        assert!(out[1].css.is_none());
        assert_eq!(out[1].hex, "#6E7075");
    }

    #[test]
    fn parses_object_with_variables_wrapper() {
        let blocks = vec![block(
            r##"{ "variables": [
  { "name": "x", "resolvedType": "COLOR", "value": "#FFFFFF",
    "codeSyntax": { "WEB": "--white" } }
] }"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].css.as_deref(), Some("--white"));
    }

    #[test]
    fn parses_rgb_floats() {
        let blocks = vec![block(
            r##"[
  { "name": "x", "resolvedType": "COLOR",
    "value": { "r": 1.0, "g": 1.0, "b": 1.0 },
    "codeSyntax": { "WEB": "--white" } }
]"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out[0].hex, "#FFFFFF");
    }

    #[test]
    fn parses_values_by_mode() {
        let blocks = vec![block(
            r##"[
  { "name": "x", "resolvedType": "COLOR",
    "valuesByMode": { "Light": "#DEF4F2" },
    "codeSyntax": { "WEB": "--blue-100" } }
]"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out[0].hex, "#DEF4F2");
    }

    #[test]
    fn skips_non_color_variables() {
        let blocks = vec![block(
            r##"[
  { "name": "spacing/md", "resolvedType": "FLOAT", "value": 16 },
  { "name": "color/x", "resolvedType": "COLOR", "value": "#000000",
    "codeSyntax": { "WEB": "--black" } }
]"##,
        )];
        let out = parse_blocks(&blocks).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].css.as_deref(), Some("--black"));
    }

    #[test]
    fn non_json_block_does_not_panic() {
        let blocks = vec![block("this is not json")];
        let err = parse_blocks(&blocks).unwrap_err();
        assert!(err.to_string().contains("no parseable JSON"));
    }

    #[test]
    fn hex_strings_normalize_correctly() {
        assert_eq!(parse_hex_string("#fff").unwrap(), "#FFFFFF");
        assert_eq!(parse_hex_string("FFFFFF").unwrap(), "#FFFFFF");
        assert_eq!(parse_hex_string("#DeAdBe").unwrap(), "#DEADBE");
        assert_eq!(parse_hex_string("#FFFFFFFF").unwrap(), "#FFFFFF"); // drops alpha
        assert!(parse_hex_string("#gg").is_none());
    }

    #[test]
    fn rgb_to_hex_rounds_correctly() {
        assert_eq!(rgb_to_hex((0.0, 0.0, 0.0)), "#000000");
        assert_eq!(rgb_to_hex((1.0, 1.0, 1.0)), "#FFFFFF");
        assert_eq!(rgb_to_hex((0.871, 0.957, 0.949)), "#DEF4F2");
    }
}
