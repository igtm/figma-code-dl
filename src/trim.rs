//! Post-process pass that trims redundant Tailwind classes and JSX attributes
//! from `figma-code-dl` output to reduce token count when the file is later
//! read by an LLM.
//!
//! Driven by a TOML config (typically `.figma/config.toml`):
//!
//! ```toml
//! [trim]
//! exclude_exact = ["relative", "shrink-0", "content-stretch"]
//! exclude_prefixes = ["inset-", "mask-"]
//! drop_empty_classname = true
//! strip_attributes = ["data-node-id", "data-name"]
//! ```

use anyhow::{Context, Result};
use regex::{Captures, Regex};
use serde::Deserialize;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct TrimConfig {
    /// Class names that must match exactly (whole token).
    #[serde(default)]
    pub exclude_exact: Vec<String>,
    /// Class names matched by `starts_with`. Matches arbitrary-value variants
    /// too (e.g. `inset-` matches `inset-[12.5%]`).
    #[serde(default)]
    pub exclude_prefixes: Vec<String>,
    /// When trimming leaves `className=""`, remove the attribute entirely.
    #[serde(default = "default_true")]
    pub drop_empty_classname: bool,
    /// JSX attributes to remove unconditionally (e.g. `data-node-id`,
    /// `data-name`). Quoted-value form (`name="..."`) only.
    #[serde(default)]
    pub strip_attributes: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Top-level TOML envelope: `[trim] ...`.
#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    trim: TrimConfig,
}

impl TrimConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read_to_string(path)
            .with_context(|| format!("reading trim config {}", path.display()))?;
        let cfg: ConfigFile =
            toml::from_str(&bytes).with_context(|| format!("parsing TOML {}", path.display()))?;
        Ok(cfg.trim)
    }
}

#[derive(Debug, Default)]
pub struct TrimReport {
    pub classes_removed: u32,
    pub attributes_removed: u32,
    pub classname_attrs_dropped: u32,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

/// Apply the trim pass. Returns the rewritten source and a report.
pub fn trim(src: &str, cfg: &TrimConfig) -> (String, TrimReport) {
    let mut report = TrimReport {
        bytes_before: src.len(),
        ..Default::default()
    };

    // 1) Filter className="..." token-by-token.
    let class_re = classname_re();
    let after_class = class_re.replace_all(src, |caps: &Captures| {
        let original = &caps[1];
        let mut kept: Vec<&str> = Vec::new();
        for tok in original.split_whitespace() {
            if cfg.exclude_exact.iter().any(|x| x == tok) {
                report.classes_removed += 1;
                continue;
            }
            if cfg
                .exclude_prefixes
                .iter()
                .any(|p| !p.is_empty() && tok.starts_with(p))
            {
                report.classes_removed += 1;
                continue;
            }
            kept.push(tok);
        }
        if kept.is_empty() && cfg.drop_empty_classname {
            report.classname_attrs_dropped += 1;
            String::new()
        } else if kept.is_empty() {
            r#"className="""#.to_string()
        } else {
            format!(r#"className="{}""#, kept.join(" "))
        }
    });

    // 2) Strip listed JSX attributes (e.g. data-node-id="..." / data-name="...").
    let mut working = after_class.into_owned();
    for attr in &cfg.strip_attributes {
        if attr.is_empty() {
            continue;
        }
        let pat = format!(r#"\s+{}="[^"]*""#, regex::escape(attr));
        let re = Regex::new(&pat).expect("static-ish regex");
        let prev_len = working.len();
        let after = re.replace_all(&working, "").into_owned();
        let count = (prev_len - after.len()) / 1.max(attr.len() + 4); // rough; refined below
        let true_count = re.find_iter(&working).count() as u32;
        let _ = count;
        report.attributes_removed += true_count;
        working = after;
    }

    // 3) Collapse whitespace artifacts introduced by attribute/class removal,
    //    WITHOUT touching leading indentation (so `  <div ...>` stays as-is).
    //    We rewrite line-by-line: keep leading whitespace verbatim, dedupe
    //    runs of 2+ spaces only in the body of the line.
    working = collapse_internal_spaces(&working);

    // 4) `<Tag >` → `<Tag>` (trailing space inside opening tag)
    let trailing = trailing_space_re();
    working = trailing.replace_all(&working, "<$tag>").into_owned();
    let trailing_self = trailing_space_self_close_re();
    working = trailing_self.replace_all(&working, "<$tag />").into_owned();

    report.bytes_after = working.len();
    (working, report)
}

fn classname_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"className="([^"]*)""#).unwrap())
}

/// Dedupe runs of 2+ ASCII spaces inside each line, but preserve the leading
/// whitespace verbatim so source indentation is not flattened.
fn collapse_internal_spaces(s: &str) -> String {
    static INNER: OnceLock<Regex> = OnceLock::new();
    let inner = INNER.get_or_init(|| Regex::new(r"  +").unwrap());

    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split_inclusive('\n') {
        if !first {
            // split_inclusive yields '\n' on each prior line already; nothing to do
        }
        first = false;
        let lead_end = line
            .find(|c: char| !matches!(c, ' ' | '\t'))
            .unwrap_or(line.len());
        let (lead, rest) = line.split_at(lead_end);
        out.push_str(lead);
        out.push_str(&inner.replace_all(rest, " "));
    }
    out
}

fn trailing_space_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<(?P<tag>[A-Za-z][A-Za-z0-9]*) +>").unwrap())
}

fn trailing_space_self_close_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<(?P<tag>[A-Za-z][A-Za-z0-9]*) +/>").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(exact: &[&str], prefixes: &[&str]) -> TrimConfig {
        TrimConfig {
            exclude_exact: exact.iter().map(|s| s.to_string()).collect(),
            exclude_prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
            drop_empty_classname: true,
            strip_attributes: vec![],
        }
    }

    #[test]
    fn exact_match_removed() {
        let src = r#"<div className="relative flex shrink-0">x</div>"#;
        let (out, r) = trim(src, &cfg(&["relative", "shrink-0"], &[]));
        assert_eq!(out, r#"<div className="flex">x</div>"#);
        assert_eq!(r.classes_removed, 2);
    }

    #[test]
    fn prefix_match_removed() {
        let src = r#"<div className="inset-[12.5%] mask-position-[-3px_-3px] flex">x</div>"#;
        let (out, r) = trim(src, &cfg(&[], &["inset-", "mask-"]));
        assert_eq!(out, r#"<div className="flex">x</div>"#);
        assert_eq!(r.classes_removed, 2);
    }

    #[test]
    fn empty_classname_dropped() {
        let src = r#"<div className="relative shrink-0">x</div>"#;
        let (out, r) = trim(src, &cfg(&["relative", "shrink-0"], &[]));
        assert_eq!(out, "<div>x</div>");
        assert_eq!(r.classname_attrs_dropped, 1);
        assert_eq!(r.classes_removed, 2);
    }

    #[test]
    fn empty_classname_kept_when_flag_off() {
        let mut c = cfg(&["a"], &[]);
        c.drop_empty_classname = false;
        let src = r#"<div className="a">x</div>"#;
        let (out, _) = trim(src, &c);
        assert_eq!(out, r#"<div className="">x</div>"#);
    }

    #[test]
    fn attribute_strip() {
        let c = TrimConfig {
            exclude_exact: vec![],
            exclude_prefixes: vec![],
            drop_empty_classname: true,
            strip_attributes: vec!["data-node-id".into(), "data-name".into()],
        };
        let src = r#"<div className="flex" data-node-id="1:2" data-name="foo">x</div>"#;
        let (out, r) = trim(src, &c);
        assert_eq!(out, r#"<div className="flex">x</div>"#);
        assert_eq!(r.attributes_removed, 2);
    }

    #[test]
    fn css_var_class_left_alone() {
        // Escaped slash in the class token must not break splitting.
        let src = r#"<div className="bg-[var(--color\/semantic\/background\/green,#def4f2)] flex">x</div>"#;
        let (out, _) = trim(src, &cfg(&[], &["mask-"]));
        assert_eq!(
            out,
            r#"<div className="bg-[var(--color\/semantic\/background\/green,#def4f2)] flex">x</div>"#
        );
    }

    #[test]
    fn self_closing_tag_cleanup() {
        let c = TrimConfig {
            exclude_exact: vec!["relative".into(), "size-full".into()],
            exclude_prefixes: vec![],
            drop_empty_classname: true,
            strip_attributes: vec!["data-node-id".into()],
        };
        let src = r#"<img alt="" className="relative size-full" data-node-id="x" src={img} />"#;
        let (out, _) = trim(src, &c);
        assert_eq!(out, r#"<img alt="" src={img} />"#);
    }

    #[test]
    fn report_byte_counts() {
        let src = r#"<div className="relative flex">x</div>"#;
        let (out, r) = trim(src, &cfg(&["relative"], &[]));
        assert_eq!(r.bytes_before, src.len());
        assert_eq!(r.bytes_after, out.len());
        assert!(r.bytes_after < r.bytes_before);
    }
}
