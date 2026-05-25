//! Substitute `<img src={imgXxx} ... />` references with reusable React icon
//! components. Runs as an optional post-process pass, after instance
//! replacement and before asset download, so that:
//!
//!   1. The `const imgXxx = "<url>"` declarations for matched icons are
//!      deleted (no download needed).
//!   2. All `<img>` usages of `imgXxx` are rewritten as `<ComponentName />`
//!      (optionally forwarding `className`).
//!   3. An `import { ComponentName } from "<module>/ComponentName";` line is
//!      added to the import block.
//!
//! Matching is driven by a TOML config (`[icons]` section in
//! `.figma/config.toml`). A const name is resolved to a component by:
//!
//!   - exact entry in `[icons.overrides]`, OR
//!   - PascalCase-derived candidate (strip `img` prefix) found by scanning
//!     `component_dir` for `*.tsx`, with optional fallback that strips trailing
//!     digits (`ChevronForward1` → `ChevronForward`).

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::imports::Imports;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct IconConfig {
    /// Filesystem directory to scan for existing icon components (`*.tsx`).
    /// When unset, only explicit `overrides` produce matches.
    pub component_dir: Option<PathBuf>,
    /// Module specifier used in generated imports. Combined with the matched
    /// component name as `<module>/<ComponentName>`. Required for matches.
    pub module: Option<String>,
    /// Export style for scanned components: `"named"` (default) or `"default"`.
    #[serde(default = "default_named")]
    pub default_export: String,
    /// When the PascalCase-derived candidate is not present in `component_dir`,
    /// retry after stripping trailing digits (`ChevronForward1` → `ChevronForward`).
    #[serde(default = "default_true")]
    pub strip_trailing_digits: bool,
    /// Forward `className` from the original `<img>` tag to the icon component.
    #[serde(default = "default_true")]
    pub forward_classname: bool,
    /// When true, any `const imgXxx = "<local-relative-path>.svg"` declaration
    /// that wasn't matched by an override or by `component_dir` scan is also
    /// converted: the URL itself becomes the import module, the component name
    /// is derived as PascalCase from `imgXxx` (`imgImage145` → `Image145`),
    /// and `<img src={imgXxx} ... />` becomes `<Image145 />`. Assumes the
    /// project's bundler returns a React component for default SVG imports
    /// (Vite + svgr, webpack + @svgr/webpack, etc.). Useless before
    /// `--download-assets` runs since the URL must be a local path.
    #[serde(default)]
    pub local_svg_import: bool,
    /// Explicit per-const overrides. Key is the const name as it appears in
    /// the output (e.g. `imgChevronForward1`).
    #[serde(default)]
    pub overrides: BTreeMap<String, OverrideValue>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum OverrideValue {
    /// `imgFoo = "ComponentName"` — uses `module` + `default_export` from
    /// the top-level config.
    Name(String),
    /// `imgFoo = { name = "X", module = "@/y", export = "default" }`.
    Full {
        name: String,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        export: Option<String>,
    },
}

fn default_named() -> String {
    "named".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    icons: IconConfig,
}

impl IconConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("reading icons config {}", path.display()))?;
        let cf: ConfigFile =
            toml::from_str(&s).with_context(|| format!("parsing TOML {}", path.display()))?;
        Ok(cf.icons)
    }
}

#[derive(Debug, Default)]
pub struct IconReport {
    /// `imgXxx` const name → number of `<img>` usages substituted.
    pub replaced: BTreeMap<String, u32>,
    /// `imgXxx` const names that no override / scan resolved.
    pub unmatched: Vec<String>,
    /// Number of `const imgXxx = "..."` lines deleted because their icon was
    /// substituted into JSX.
    pub const_decls_removed: u32,
}

pub struct IconOutput {
    pub body: String,
    pub imports: Imports,
    pub report: IconReport,
}

#[derive(Debug, Clone)]
struct Resolved {
    component: String,
    module: String,
    export: String,
}

pub fn process(src: &str, cfg: &IconConfig) -> Result<IconOutput> {
    let mut report = IconReport::default();
    let mut imports = Imports::new();

    // 1) Inventory of existing icon component file stems.
    let scanned: HashSet<String> = match &cfg.component_dir {
        Some(dir) => scan_components(dir)?,
        None => HashSet::new(),
    };

    // 2) Find every `const imgXxx = "..." ;` declaration.
    let const_re = const_decl_re();
    let mut decls: Vec<(usize, usize, String)> = Vec::new();
    for cap in const_re.captures_iter(src) {
        let full = cap.get(0).unwrap();
        let name = cap.get(1).unwrap().as_str().to_string();
        decls.push((full.start(), full.end(), name));
    }

    // 3) Resolve each const to a component (override > scan > local-svg).
    //    Build a name -> url map first so the local-svg branch can inspect
    //    each const's value.
    let const_url: BTreeMap<&str, String> = const_urls(src);

    let mut resolved: BTreeMap<String, Resolved> = BTreeMap::new();
    let mut unmatched: Vec<String> = Vec::new();
    for (_, _, name) in &decls {
        if resolved.contains_key(name) {
            continue;
        }
        if let Some(r) = resolve(name, cfg, &scanned) {
            resolved.insert(name.clone(), r);
            continue;
        }
        if cfg.local_svg_import
            && let Some(url) = const_url.get(name.as_str())
            && is_local_svg_path(url)
        {
            let component = pascal_from_img_const(name);
            resolved.insert(
                name.clone(),
                Resolved {
                    component,
                    module: url.clone(),
                    export: "default".to_string(),
                },
            );
            continue;
        }
        unmatched.push(name.clone());
    }
    report.unmatched = unmatched;

    // 4) Build the ops list: rewrite <img> usages, then delete const decls.
    struct Op {
        start: usize,
        end: usize,
        replacement: String,
    }
    let mut ops: Vec<Op> = Vec::new();

    for cap in img_tag_re().captures_iter(src) {
        let full = cap.get(0).unwrap();
        let attrs = cap.get(1).unwrap().as_str();

        let Some(src_var) = extract_src_var(attrs) else {
            continue;
        };
        let Some(r) = resolved.get(src_var) else {
            continue;
        };

        let class = if cfg.forward_classname {
            extract_classname(attrs)
        } else {
            None
        };
        let replacement = match class {
            Some(c) if !c.is_empty() => format!(r#"<{} className="{}" />"#, r.component, c),
            _ => format!("<{} />", r.component),
        };

        ops.push(Op {
            start: full.start(),
            end: full.end(),
            replacement,
        });
        imports.add(&r.module, &r.export, &r.component);
        *report.replaced.entry(src_var.to_string()).or_default() += 1;
    }

    // Delete a `const imgXxx = "..."` when (a) it resolved to a component
    // AND (b) it has no surviving references outside `<img src={imgXxx}>`
    // patterns. That handles three cases:
    //   - All usages were `<img>` and got substituted (the common case).
    //   - The const has zero `<img>` usages because an earlier pass (e.g.
    //     `--map`) already replaced the wrapping subtree — it's now an
    //     orphan dead-letter.
    //   - The const is referenced by `style={{ maskImage: `url('${...}')` }}`
    //     or template literals → KEEP (those would dangle).
    for (start, end, name) in &decls {
        if !resolved.contains_key(name) {
            continue;
        }
        if has_non_img_reference(src, name) {
            continue;
        }
        let mut end_x = *end;
        if src.as_bytes().get(end_x) == Some(&b'\n') {
            end_x += 1;
        }
        ops.push(Op {
            start: *start,
            end: end_x,
            replacement: String::new(),
        });
        report.const_decls_removed += 1;
    }

    // 5) Apply ops in reverse so earlier offsets stay stable.
    ops.sort_by_key(|op| std::cmp::Reverse(op.start));
    let mut body = src.to_string();
    for op in ops {
        body.replace_range(op.start..op.end, &op.replacement);
    }

    Ok(IconOutput {
        body,
        imports,
        report,
    })
}

fn resolve(const_name: &str, cfg: &IconConfig, scanned: &HashSet<String>) -> Option<Resolved> {
    if let Some(ov) = cfg.overrides.get(const_name) {
        let (name, module_override, export_override) = match ov {
            OverrideValue::Name(n) => (n.clone(), None, None),
            OverrideValue::Full {
                name,
                module,
                export,
            } => (name.clone(), module.clone(), export.clone()),
        };
        let module_prefix = module_override.or_else(|| cfg.module.clone())?;
        let export_style = export_override.unwrap_or_else(|| cfg.default_export.clone());
        return Some(make_resolved(&name, &module_prefix, &export_style));
    }

    // Auto-derive from the const name.
    let module = cfg.module.as_ref()?;
    let candidate = derive_candidate(const_name);

    if scanned.contains(&candidate) {
        return Some(make_resolved(&candidate, module, &cfg.default_export));
    }
    if cfg.strip_trailing_digits {
        let stripped = strip_trailing_digits(&candidate);
        if stripped != candidate && scanned.contains(&stripped) {
            return Some(make_resolved(&stripped, module, &cfg.default_export));
        }
    }
    None
}

/// Build a `Resolved` from `(component_name, module_prefix, export_style)`.
/// `export_style` is `"default"` or anything else (e.g. `"named"`). For
/// non-default the component name itself is used as the named export, matching
/// the common one-component-per-file convention.
fn make_resolved(component: &str, module_prefix: &str, export_style: &str) -> Resolved {
    let export = if export_style == "default" {
        "default".to_string()
    } else {
        component.to_string()
    };
    Resolved {
        component: component.to_string(),
        module: format!("{}/{}", module_prefix.trim_end_matches('/'), component),
        export,
    }
}

/// PascalCase candidate from `imgXxx` (strip the `img` prefix). The const name
/// is already PascalCase after `img`, so no further transformation is needed.
fn derive_candidate(const_name: &str) -> String {
    const_name
        .strip_prefix("img")
        .unwrap_or(const_name)
        .to_string()
}

fn strip_trailing_digits(s: &str) -> String {
    s.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
}

fn scan_components(dir: &Path) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    if !dir.exists() {
        return Ok(set);
    }
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading components dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Allow `Foo/index.tsx` pattern: the directory name is the component name.
            if path.join("index.tsx").exists()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                set.insert(name.to_string());
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("tsx")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            set.insert(stem.to_string());
        }
    }
    Ok(set)
}

// --- Regex helpers ---------------------------------------------------------

fn const_decl_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"const\s+(img[A-Za-z0-9_]+)\s*=\s*"[^"]+"\s*;"#).unwrap())
}

/// Walk `const imgXxx = "<url>";` declarations and return `name -> url`.
fn const_urls(src: &str) -> BTreeMap<&str, String> {
    static FULL_RE: OnceLock<Regex> = OnceLock::new();
    let re = FULL_RE
        .get_or_init(|| Regex::new(r#"const\s+(img[A-Za-z0-9_]+)\s*=\s*"([^"]+)"\s*;"#).unwrap());
    re.captures_iter(src)
        .filter_map(|cap| {
            let name = cap.get(1)?.as_str();
            let url = cap.get(2)?.as_str().to_string();
            Some((name, url))
        })
        .collect()
}

fn is_local_svg_path(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !lower.ends_with(".svg") {
        return false;
    }
    // Treat anything that isn't an absolute http(s) URL as local. Covers
    // `./...`, `../...`, `/abs/...`, and bare `assets/...` forms.
    !(lower.starts_with("http://") || lower.starts_with("https://"))
}

/// `imgImage145` → `Image145`. Numbers and underscores are kept verbatim;
/// the `img` prefix is the only thing stripped.
fn pascal_from_img_const(const_name: &str) -> String {
    const_name
        .strip_prefix("img")
        .unwrap_or(const_name)
        .to_string()
}

fn img_tag_re() -> &'static Regex {
    // Match `<img ... />` or `<img ... >`. Attrs in group 1.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<img\b([^>]*?)/?>").unwrap())
}

fn extract_src_var(attrs: &str) -> Option<&str> {
    let needle = "src={";
    let pos = attrs.find(needle)?;
    let rest = &attrs[pos + needle.len()..];
    let end = rest.find('}')?;
    let inside = rest[..end].trim();
    if !inside.is_empty()
        && inside
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some(inside)
    } else {
        None
    }
}

/// Returns true if `name` appears in `src` outside of `<img ... src={name}>`
/// patterns — i.e. somewhere that would dangle if the `const name = "..."`
/// declaration were deleted. Used to decide whether to remove the decl.
fn has_non_img_reference(src: &str, name: &str) -> bool {
    let mut pos = 0;
    while let Some(rel) = src[pos..].find(name) {
        let abs = pos + rel;
        // Word boundaries: char before/after must not be identifier-like.
        let before_ok = abs == 0
            || !src.as_bytes()[abs - 1].is_ascii_alphanumeric() && src.as_bytes()[abs - 1] != b'_';
        let after_off = abs + name.len();
        let after_ok = after_off >= src.len()
            || (!src.as_bytes()[after_off].is_ascii_alphanumeric()
                && src.as_bytes()[after_off] != b'_');
        if !before_ok || !after_ok {
            pos = abs + name.len();
            continue;
        }

        // Skip its own declaration: `const <name> = ...`.
        let prefix = &src[..abs].trim_end_matches(' ');
        if prefix.ends_with("const") {
            pos = abs + name.len();
            continue;
        }

        // Determine if this reference is inside an <img ... src={<name>}>.
        // Look backwards from `abs` for the nearest `<` and check whether it's
        // followed by `img` and the reference is positioned as `src={name}`.
        let before = &src[..abs];
        if let Some(lt) = before.rfind('<') {
            let between = &src[lt..abs];
            let next_gt = src[lt..].find('>').map(|g| lt + g);
            // If there's a `>` before our position, we're outside any tag.
            let inside_tag = next_gt.map(|g| g > abs).unwrap_or(false);
            if inside_tag && between.starts_with("<img") {
                // Make sure we're inside a `src={...}` braces region containing
                // exactly this identifier.
                let inner = &src[lt..abs + name.len()];
                if inner.contains("src={")
                    && let Some(brace_open) = inner.rfind("src={")
                {
                    let brace_payload = &inner[brace_open + "src={".len()..];
                    if brace_payload.trim() == name {
                        // This is an <img src={name}>; we'll have rewritten it.
                        pos = abs + name.len();
                        continue;
                    }
                }
            }
        }

        return true;
    }
    false
}

fn extract_classname(attrs: &str) -> Option<String> {
    let needle = r#"className=""#;
    let pos = attrs.find(needle)?;
    let rest = &attrs[pos + needle.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_minimal(module: &str, overrides: &[(&str, &str)]) -> IconConfig {
        IconConfig {
            component_dir: None,
            module: Some(module.into()),
            default_export: "named".into(),
            strip_trailing_digits: true,
            forward_classname: true,
            local_svg_import: false,
            overrides: overrides
                .iter()
                .map(|(k, v)| (k.to_string(), OverrideValue::Name(v.to_string())))
                .collect(),
        }
    }

    #[test]
    fn override_substitutes_img() {
        let src = r#"const imgFoo = "https://x/y.svg";
<img alt="" className="size-[24px]" src={imgFoo} />
"#;
        let cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains(r#"<Foo className="size-[24px]" />"#));
        assert!(!out.body.contains("const imgFoo"));
        assert_eq!(out.report.replaced.get("imgFoo"), Some(&1));
        assert_eq!(out.report.const_decls_removed, 1);
    }

    #[test]
    fn classname_dropped_when_disabled() {
        let src = r#"const imgFoo = "x";
<img src={imgFoo} className="big" />
"#;
        let mut cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        cfg.forward_classname = false;
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("<Foo />"));
        assert!(!out.body.contains("className"));
    }

    #[test]
    fn multiple_usages_one_import() {
        let src = r#"const imgFoo = "x";
<img src={imgFoo} />
<div><img alt="" src={imgFoo} /></div>
"#;
        let cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        let out = process(src, &cfg).unwrap();
        assert_eq!(out.report.replaced.get("imgFoo"), Some(&2));
        let rendered = out.imports.render();
        assert_eq!(rendered.matches("import").count(), 1);
        assert!(rendered.contains(r#"import { Foo } from "@/icons/Foo";"#));
    }

    #[test]
    fn unmatched_left_alone() {
        let src = r#"const imgKnown = "x";
const imgUnknown = "y";
<img src={imgKnown} />
<img src={imgUnknown} />
"#;
        let cfg = cfg_minimal("@/icons", &[("imgKnown", "Known")]);
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("<Known />"));
        assert!(out.body.contains("const imgUnknown"));
        assert!(out.body.contains("src={imgUnknown}"));
        assert_eq!(out.report.unmatched, vec!["imgUnknown".to_string()]);
    }

    #[test]
    fn full_override_with_module_and_export() {
        let src = r#"const imgFoo = "x";
<img src={imgFoo} />
"#;
        let mut cfg = cfg_minimal("@/icons", &[]);
        cfg.overrides.insert(
            "imgFoo".into(),
            OverrideValue::Full {
                name: "Foo".into(),
                module: Some("@/special".into()),
                export: Some("default".into()),
            },
        );
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("<Foo />"));
        let r = out.imports.render();
        assert!(r.contains(r#"import Foo from "@/special/Foo";"#));
    }

    #[test]
    fn default_export_style() {
        let src = r#"const imgFoo = "x";
<img src={imgFoo} />
"#;
        let mut cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        cfg.default_export = "default".into();
        let out = process(src, &cfg).unwrap();
        assert!(
            out.imports
                .render()
                .contains(r#"import Foo from "@/icons/Foo";"#)
        );
    }

    #[test]
    fn scan_with_strip_trailing_digits() {
        let tmp = std::env::temp_dir().join(format!("figma-code-dl-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("ChevronForward.tsx"), "").unwrap();

        let src = r#"const imgChevronForward1 = "x";
<img src={imgChevronForward1} />
"#;
        let cfg = IconConfig {
            component_dir: Some(tmp.clone()),
            module: Some("@/icons".into()),
            default_export: "named".into(),
            strip_trailing_digits: true,
            forward_classname: true,
            local_svg_import: false,
            overrides: Default::default(),
        };
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("<ChevronForward />"));
        assert!(
            out.imports
                .render()
                .contains(r#"import { ChevronForward } from "@/icons/ChevronForward";"#)
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn no_module_no_match() {
        let src = r#"const imgFoo = "x";
<img src={imgFoo} />
"#;
        let cfg = IconConfig {
            component_dir: None,
            module: None,
            default_export: "named".into(),
            strip_trailing_digits: true,
            forward_classname: true,
            local_svg_import: false,
            overrides: Default::default(),
        };
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("const imgFoo"));
        assert!(out.body.contains("src={imgFoo}"));
        assert_eq!(out.report.unmatched, vec!["imgFoo".to_string()]);
    }

    #[test]
    fn keeps_const_when_referenced_outside_img() {
        // Figma emits a mask-image pattern that references the URL constant
        // in a style block. Even if `imgFoo` is mapped to a component, the
        // constant must stay defined because the style block still uses it.
        let src = r#"const imgFoo = "x";
<div style={{ maskImage: `url('${imgFoo}')` }}>
  <span />
</div>
"#;
        let cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        let out = process(src, &cfg).unwrap();
        // The const decl must remain because of the style ref.
        assert!(out.body.contains("const imgFoo"));
        // No <img> was substituted, but the resolver still saw `imgFoo`.
        assert_eq!(out.report.replaced.get("imgFoo"), None);
        assert_eq!(out.report.const_decls_removed, 0);
    }

    #[test]
    fn deletes_const_only_when_all_uses_substituted() {
        // `imgFoo` is used both in an <img> (substituted) and in a style block
        // (kept). The decl must NOT be deleted.
        let src = r#"const imgFoo = "x";
<img src={imgFoo} />
<div style={{ maskImage: `url('${imgFoo}')` }} />
"#;
        let cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("const imgFoo"));
        assert!(out.body.contains("<Foo />"));
        assert!(out.body.contains("maskImage:"));
        assert_eq!(out.report.const_decls_removed, 0);
    }

    #[test]
    fn deletes_orphan_const_with_no_remaining_references() {
        // Mirrors what happens when --map replaces an instance subtree
        // BEFORE --icons runs: the <img src={imgFoo}> tags inside the
        // subtree are gone, so icons sees the const + no usages. The const
        // should still be deleted as a dead reference.
        let src = r#"const imgFoo = "./assets/foo.svg";
<div>some unrelated content</div>"#;
        let cfg = cfg_minimal("@/icons", &[("imgFoo", "Foo")]);
        let out = process(src, &cfg).unwrap();
        assert!(!out.body.contains("const imgFoo"));
        assert_eq!(out.report.const_decls_removed, 1);
        assert!(out.imports.render().is_empty());
    }

    #[test]
    fn deletes_const_when_all_imgs_substituted_real_world() {
        // Mirrors the actual figma-code-dl output: const declared on its
        // own line, multiple <img src={imgStylus}/> in deeply-nested JSX.
        // The outer <div> uses a DIFFERENT identifier (imgDownload) for its
        // maskImage, so imgStylus has 0 non-img references and the const
        // must be deleted.
        let src = r##"const imgStylus = "https://x";
const imgDownload = "https://y";
<div className="absolute" data-node-id="A:1" style={{ maskImage: `url('${imgDownload}')` }} data-name="stylus">
  <img alt="" className="absolute block inset-0 max-w-none size-full" src={imgStylus} />
</div>
<div className="absolute" data-node-id="A:2" style={{ maskImage: `url('${imgDownload}')` }} data-name="stylus">
  <img alt="" className="absolute block inset-0 max-w-none size-full" src={imgStylus} />
</div>"##;
        let cfg = cfg_minimal("@/icons", &[("imgStylus", "Stylus")]);
        let out = process(src, &cfg).unwrap();
        assert!(
            !out.body.contains("const imgStylus"),
            "expected `const imgStylus` deleted, got:\n{}",
            out.body
        );
        // imgDownload is still used in maskImage → must remain.
        assert!(out.body.contains("const imgDownload"));
        assert_eq!(out.report.replaced.get("imgStylus"), Some(&2));
    }

    #[test]
    fn local_svg_imports_unmapped_svg() {
        let src = r#"const imgFoo = "./assets/foo.svg";
const imgKnown = "./assets/known.svg";
const imgPng = "./assets/photo.png";
<img alt="" className="size-[24px]" src={imgFoo} />
<img src={imgKnown} />
<img src={imgPng} />
"#;
        let mut cfg = cfg_minimal("@/icons", &[("imgKnown", "Known")]);
        cfg.local_svg_import = true;
        let out = process(src, &cfg).unwrap();

        // Override entry: handled by the DS path.
        assert!(out.body.contains("<Known />"));
        // Local-SVG entry: handled by the auto path, default import from local URL.
        assert!(out.body.contains(r#"<Foo className="size-[24px]" />"#));
        assert!(out.body.contains("const imgPng")); // PNG left as-is
        let imports = out.imports.render();
        assert!(imports.contains(r#"import Foo from "./assets/foo.svg";"#));
        assert!(imports.contains(r#"import { Known } from "@/icons/Known";"#));
    }

    #[test]
    fn local_svg_off_keeps_unmatched_const() {
        let src = r#"const imgFoo = "./assets/foo.svg";
<img src={imgFoo} />
"#;
        let cfg = cfg_minimal("@/icons", &[]);
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("const imgFoo"));
        assert_eq!(out.report.unmatched, vec!["imgFoo".to_string()]);
    }

    #[test]
    fn local_svg_skips_remote_urls() {
        // Before --download-assets runs, URLs are cloud URLs without a .svg
        // extension. local_svg_import must NOT match those.
        let src = r#"const imgFoo = "https://www.figma.com/api/mcp/asset/uuid";
<img src={imgFoo} />
"#;
        let mut cfg = cfg_minimal("@/icons", &[]);
        cfg.local_svg_import = true;
        let out = process(src, &cfg).unwrap();
        assert!(out.body.contains("const imgFoo"));
        assert_eq!(out.report.unmatched, vec!["imgFoo".to_string()]);
    }

    #[test]
    fn loads_from_toml() {
        let tmp = std::env::temp_dir().join(format!(
            "figma-code-dl-iconscfg-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &tmp,
            r#"
[icons]
component_dir = "src/icons"
module = "@/icons"
default_export = "named"
strip_trailing_digits = true
forward_classname = true

[icons.overrides]
imgChevronForward1 = "ChevronForward"
imgLogo = { name = "Logo", module = "@/branding", export = "default" }
"#,
        )
        .unwrap();
        let cfg = IconConfig::load(&tmp).unwrap();
        assert_eq!(cfg.module.as_deref(), Some("@/icons"));
        assert!(matches!(
            cfg.overrides.get("imgChevronForward1"),
            Some(OverrideValue::Name(n)) if n == "ChevronForward"
        ));
        assert!(matches!(
            cfg.overrides.get("imgLogo"),
            Some(OverrideValue::Full { name, module: Some(m), export: Some(e) })
                if name == "Logo" && m == "@/branding" && e == "default"
        ));
        std::fs::remove_file(&tmp).ok();
    }
}
