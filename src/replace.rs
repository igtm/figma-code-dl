//! JSX-level instance replacement on the TSX output produced by
//! `get_design_context`. The Figma server preserves a `data-name` attribute on
//! every JSX element matching a Figma layer/component name; we replace
//! subtrees whose `data-name` (or `data-node-id`) hits the mapping with a
//! `<LocalComponent />` reference and inject the corresponding `import`.
//!
//! Top-level `function Foo(…)` declarations that the server emitted for
//! heavily-reused instances are also removed when `Foo` matches a mapping
//! key; the existing `<Foo />` call sites resolve to the injected import.

use anyhow::{Result, anyhow};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use crate::imports::Imports;
use crate::instance_map::InstanceMap;

#[derive(Debug, Default)]
pub struct Report {
    /// Mapping key (data-name or node id) → number of replacements made.
    pub replaced: BTreeMap<String, u32>,
    /// Inlined instance data-name → count of occurrences not matched by any
    /// mapping. Useful for `--report-unmapped`.
    pub unmapped: BTreeMap<String, u32>,
    /// Server-extracted function names that were removed.
    pub removed_functions: Vec<String>,
}

pub struct Output {
    /// Coalesced import set. Render with `.render()` (or merge with imports
    /// produced by other passes first).
    pub imports: Imports,
    pub body: String,
    pub report: Report,
}

pub fn process(code: &str, map: &InstanceMap) -> Result<Output> {
    let mut report = Report::default();
    let mut imports = Imports::new();

    // -------- Pass 1: JSX subtree replacements -------------------------------
    let candidates = find_inline_instance_candidates(code);

    #[derive(Debug)]
    struct Op {
        start: usize,
        end: usize,
        replacement: String,
    }
    let mut ops: Vec<Op> = Vec::new();

    for cand in &candidates {
        let resolved = map
            .by_node_id
            .get(cand.data_node_id)
            .map(|e| (e, InstanceMap::resolve_binding(e, cand.data_name)))
            .or_else(|| {
                map.mappings
                    .get(cand.data_name)
                    .map(|e| (e, InstanceMap::resolve_binding(e, cand.data_name)))
            });

        let Some((entry, local)) = resolved else {
            *report
                .unmapped
                .entry(cand.data_name.to_string())
                .or_default() += 1;
            continue;
        };

        let end = if cand.self_close {
            cand.tag_end
        } else {
            find_subtree_end(code, cand.tag_end, cand.tag).ok_or_else(|| {
                anyhow!(
                    "could not find closing </{}> for opening tag at byte {}",
                    cand.tag,
                    cand.tag_start
                )
            })?
        };

        ops.push(Op {
            start: cand.tag_start,
            end,
            replacement: format!("<{local} />"),
        });
        imports.add(&entry.module, &entry.export, &local);
        *report
            .replaced
            .entry(cand.data_name.to_string())
            .or_default() += 1;
    }

    // -------- Pass 2: warn on name collisions --------------------------------
    let mut by_name_inner_signature: HashMap<&str, &str> = HashMap::new();
    for cand in &candidates {
        if let Some(prev) = by_name_inner_signature.get(cand.data_name) {
            if *prev != cand.data_node_id {
                // Different node ids under the same data-name => potentially
                // different Figma components. We still replace (per spec) but
                // surface the ambiguity so the user knows to set `byNodeId`.
                if map.mappings.contains_key(cand.data_name) {
                    eprintln!(
                        "WARN: data-name=\"{}\" matched multiple distinct node ids \
                         ({}, {}, ...); all replaced with the same mapping. \
                         Use `byNodeId` to disambiguate.",
                        cand.data_name, prev, cand.data_node_id
                    );
                }
            }
        } else {
            by_name_inner_signature.insert(cand.data_name, cand.data_node_id);
        }
    }

    // -------- Pass 3: remove server-extracted top-level functions ------------
    for (key, entry) in &map.mappings {
        if let Some((start, end)) = find_top_level_function(code, key) {
            ops.push(Op {
                start,
                end,
                replacement: String::new(),
            });
            // Crucial: bind the import under the SAME identifier as the function
            // we just deleted, so existing `<Foo />` call sites keep resolving.
            imports.add(&entry.module, &entry.export, key);
            report.removed_functions.push(key.clone());
        }
    }

    // -------- De-overlap, keeping the outer op when one is nested in another.
    //
    // Common case: a `function Foo(…)` removal op encloses a JSX-replacement
    // op derived from `<div data-name="Foo">` inside its body. The outer
    // op (the function removal) should win because the inner range is
    // disappearing anyway.
    let mut sorted = ops;
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept: Vec<Op> = Vec::new();
    let mut next_min_start = 0usize;
    for op in sorted {
        if op.start < next_min_start {
            continue; // nested within a previously-kept (outer) op
        }
        next_min_start = op.end;
        kept.push(op);
    }

    // Apply in reverse start order so earlier byte offsets stay stable.
    kept.sort_by_key(|op| std::cmp::Reverse(op.start));
    let mut body = code.to_string();
    for op in kept {
        body.replace_range(op.start..op.end, &op.replacement);
    }

    Ok(Output {
        imports,
        body,
        report,
    })
}

// ---------------------------------------------------------------------------
// Candidate detection
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Candidate<'a> {
    /// Byte index of the opening `<`.
    tag_start: usize,
    /// Byte index immediately after the opening tag's closing `>`.
    tag_end: usize,
    tag: &'a str,
    data_name: &'a str,
    data_node_id: &'a str,
    self_close: bool,
}

/// Match an opening JSX tag like `<div ...>` or `<div .../>`. We disallow `<`
/// and `>` inside the attribute span, which holds for the auto-generated
/// Figma output (none of its JSX expressions contain literal `<`/`>` inside
/// braces).
fn opening_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<([A-Za-z][A-Za-z0-9]*)\b([^<>]*)>").unwrap())
}

fn find_inline_instance_candidates(code: &str) -> Vec<Candidate<'_>> {
    let mut out = Vec::new();
    for m in opening_tag_re().captures_iter(code) {
        let full = m.get(0).unwrap();
        let tag = m.get(1).unwrap().as_str();
        let attrs = m.get(2).unwrap().as_str();

        let Some(data_name) = attr_value(attrs, "data-name") else {
            continue;
        };
        let Some(data_node_id) = attr_value(attrs, "data-node-id") else {
            continue;
        };

        // Inlined-instance roots have a bare node id. Sub-nodes of an inlined
        // instance get IDs like `I<root>;<sub>` which we deliberately skip.
        if data_node_id.contains(';') {
            continue;
        }

        let self_close = attrs.trim_end().ends_with('/');
        out.push(Candidate {
            tag_start: full.start(),
            tag_end: full.end(),
            tag,
            data_name,
            data_node_id,
            self_close,
        });
    }
    out
}

fn attr_value<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    // Find ` <name>="` with a word-boundary on the left.
    let needle = format!(r#"{name}=""#);
    let bytes = attrs.as_bytes();
    let mut from = 0;
    while let Some(off) = attrs[from..].find(&needle) {
        let abs = from + off;
        let boundary_ok =
            abs == 0 || matches!(bytes[abs - 1], b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'>');
        if boundary_ok {
            let value_start = abs + needle.len();
            let value_end = attrs[value_start..].find('"')?;
            return Some(&attrs[value_start..value_start + value_end]);
        }
        from = abs + needle.len();
    }
    None
}

// ---------------------------------------------------------------------------
// Subtree end detection (depth tracking)
// ---------------------------------------------------------------------------

/// Find the byte index immediately after the matching `</tag>` for the
/// opening tag whose body starts at `body_start`.
fn find_subtree_end(code: &str, body_start: usize, tag: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth: i32 = 1;
    let mut pos = body_start;

    while pos < code.len() {
        let lt_off = bytes[pos..].iter().position(|&b| b == b'<')?;
        let lt = pos + lt_off;
        let rest = &code[lt..];

        if is_closing_tag(rest, tag) {
            depth -= 1;
            let close_len = 2 + tag.len() + 1; // "</" + tag + ">"
            pos = lt + close_len;
            if depth == 0 {
                return Some(pos);
            }
        } else if is_opening_tag_named(rest, tag) {
            let gt_off = rest.find('>')?;
            let tag_end = lt + gt_off + 1;
            // Self-closing if the byte right before `>` (after trimming) is `/`.
            let inner = &rest[..gt_off];
            let self_close = inner.trim_end().ends_with('/');
            if !self_close {
                depth += 1;
            }
            pos = tag_end;
        } else {
            pos = lt + 1;
        }
    }
    None
}

fn is_closing_tag(rest: &str, tag: &str) -> bool {
    if !rest.starts_with("</") {
        return false;
    }
    let after = &rest[2..];
    if !after.starts_with(tag) {
        return false;
    }
    let next = after.as_bytes().get(tag.len()).copied();
    matches!(
        next,
        Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
    )
}

fn is_opening_tag_named(rest: &str, tag: &str) -> bool {
    if !rest.starts_with('<') || rest.starts_with("</") {
        return false;
    }
    let after = &rest[1..];
    if !after.starts_with(tag) {
        return false;
    }
    let next = after.as_bytes().get(tag.len()).copied();
    match next {
        Some(c) => !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Top-level function declaration removal
// ---------------------------------------------------------------------------

fn find_top_level_function(code: &str, name: &str) -> Option<(usize, usize)> {
    let pattern = format!("function {name}(");
    let mut search_from = 0;
    while let Some(off) = code[search_from..].find(&pattern) {
        let abs = search_from + off;
        let at_line_start = abs == 0 || code.as_bytes()[abs - 1] == b'\n';
        if at_line_start {
            // Find the body's opening `{` (after the params/return-type).
            let after_pattern = abs + pattern.len();
            let open = find_outer_brace(code, after_pattern)?;
            let close = find_matching_brace(code, open)?;
            // Consume trailing newline if present so we don't leave a blank gap.
            let mut end = close + 1;
            if code.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
            }
            return Some((abs, end));
        }
        search_from = abs + pattern.len();
    }
    None
}

fn find_outer_brace(code: &str, from: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut paren_depth: i32 = 1; // we start right after the `(`
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'{' if paren_depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn find_matching_brace(code: &str, open_idx: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string: Option<u8> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut i = open_idx;
    while i < bytes.len() {
        let b = bytes[i];

        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(quote) = in_string {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => in_string = Some(b),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                in_line_comment = true;
                i += 2;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                in_block_comment = true;
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_map::{Entry, InstanceMap, Options};

    fn map_from(entries: &[(&str, Entry)]) -> InstanceMap {
        InstanceMap {
            mappings: entries
                .iter()
                .cloned()
                .map(|(k, v)| (k.into(), v))
                .collect(),
            by_node_id: Default::default(),
            options: Options::default(),
        }
    }

    fn entry(module: &str, export: &str) -> Entry {
        Entry {
            module: module.into(),
            export: export.into(),
            alias: None,
            preserve_children: false,
        }
    }

    #[test]
    fn replaces_single_subtree_and_imports_named() {
        let code = r#"
<div data-node-id="1:1" data-name="root">
  <div data-node-id="1:2" data-name="Switch">
    <div data-node-id="I1:2;9:9">inner</div>
  </div>
</div>
"#;
        let map = map_from(&[("Switch", entry("@/ds/Switch", "Switch"))]);
        let out = process(code, &map).unwrap();
        assert!(out.body.contains("<Switch />"));
        assert!(!out.body.contains("I1:2;9:9"));
        assert!(
            out.imports
                .render()
                .contains(r#"import { Switch } from "@/ds/Switch";"#)
        );
        assert_eq!(out.report.replaced.get("Switch"), Some(&1));
    }

    #[test]
    fn replaces_self_closing_tag() {
        let code = r#"<div data-node-id="1:2" data-name="Switch" />"#;
        let map = map_from(&[("Switch", entry("@/x", "Switch"))]);
        let out = process(code, &map).unwrap();
        assert_eq!(out.body.trim(), "<Switch />");
    }

    #[test]
    fn skips_subnodes_of_inlined_instance() {
        // The Switch root has bare id, its children have I-prefixed ids.
        // Only the root should be replaced.
        let code = r#"<div data-node-id="1:2" data-name="Switch">
  <div data-node-id="I1:2;3:3" data-name="thumb"></div>
</div>"#;
        let map = map_from(&[
            ("Switch", entry("@/x", "Switch")),
            ("thumb", entry("@/y", "Thumb")),
        ]);
        let out = process(code, &map).unwrap();
        assert!(out.body.contains("<Switch />"));
        assert!(!out.body.contains("data-name=\"thumb\""));
        assert_eq!(out.report.replaced.get("thumb"), None);
    }

    #[test]
    fn default_export_uses_sanitized_local() {
        let code = r#"<div data-node-id="1:2" data-name="system/person_add" />"#;
        let map = map_from(&[("system/person_add", entry("@/icons/PersonAdd", "default"))]);
        let out = process(code, &map).unwrap();
        assert!(out.body.contains("<SystemPersonAdd />"));
        assert!(
            out.imports
                .render()
                .contains(r#"import SystemPersonAdd from "@/icons/PersonAdd";"#)
        );
    }

    #[test]
    fn alias_overrides_export_name_in_import() {
        let code = r#"<div data-node-id="1:2" data-name="Switch" />"#;
        let map = map_from(&[(
            "Switch",
            Entry {
                module: "@/x".into(),
                export: "Switch".into(),
                alias: Some("DSSwitch".into()),
                preserve_children: false,
            },
        )]);
        let out = process(code, &map).unwrap();
        assert!(out.body.contains("<DSSwitch />"));
        assert!(
            out.imports
                .render()
                .contains(r#"import { Switch as DSSwitch } from "@/x";"#)
        );
    }

    #[test]
    fn unmapped_data_names_are_counted() {
        let code = r#"
<div data-node-id="1:2" data-name="Foo" />
<div data-node-id="1:3" data-name="Foo" />
<div data-node-id="1:4" data-name="Bar" />
"#;
        let map = map_from(&[]); // empty
        let out = process(code, &map).unwrap();
        assert_eq!(out.report.unmapped.get("Foo"), Some(&2));
        assert_eq!(out.report.unmapped.get("Bar"), Some(&1));
        assert!(out.imports.render().is_empty());
    }

    #[test]
    fn by_node_id_takes_precedence_over_name() {
        let code = r#"<div data-node-id="1:9" data-name="Switch" />"#;
        let mut map = map_from(&[("Switch", entry("@/general/Switch", "Switch"))]);
        map.by_node_id
            .insert("1:9".into(), entry("@/special/Custom", "Custom"));
        let out = process(code, &map).unwrap();
        assert!(out.body.contains("<Custom />"));
        assert!(
            out.imports
                .render()
                .contains(r#"import { Custom } from "@/special/Custom";"#)
        );
    }

    #[test]
    fn removes_top_level_function_when_mapped() {
        let code = "function Button({ className }: { className?: string }) {\n  return <div className={className} data-node-id=\"x\" data-name=\"Button\">btn</div>;\n}\n\nexport default function Page() {\n  return <Button className=\"foo\" />;\n}\n";
        let map = map_from(&[("Button", entry("@/ds/Button", "default"))]);
        let out = process(code, &map).unwrap();
        assert!(!out.body.contains("function Button"));
        assert!(out.body.contains("<Button className=\"foo\" />"));
        assert!(
            out.imports
                .render()
                .contains(r#"import Button from "@/ds/Button";"#)
        );
        assert!(out.report.removed_functions.contains(&"Button".to_string()));
    }

    #[test]
    fn finds_subtree_end_with_nested_same_tag() {
        let code = "<div><div>x</div></div>";
        // body_start = index after the first `<div>` (5)
        assert_eq!(find_subtree_end(code, 5, "div"), Some(code.len()));
    }

    #[test]
    fn finds_subtree_end_with_self_closing_nested() {
        let code = r#"<div><img /><div></div></div>"#;
        assert_eq!(find_subtree_end(code, 5, "div"), Some(code.len()));
    }

    // Imports combine-per-module test lives in src/imports.rs now.
}
