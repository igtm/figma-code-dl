use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// `.figma/instance-map.json` schema.
///
/// JSON shape:
/// ```jsonc
/// {
///   "mappings": {
///     "Switch":      { "module": "@/components/ds/Switch",   "export": "Switch" },
///     "DropdownBox": { "module": "@/components/ds/Dropdown", "export": "Dropdown", "alias": "Dropdown" }
///   },
///   "byNodeId": {
///     "1:2": { "module": "@/components/PageHeader", "export": "default" }
///   },
///   "options": {
///     "passClassName": false
///   }
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstanceMap {
    #[serde(default)]
    pub mappings: HashMap<String, Entry>,
    #[serde(default)]
    pub by_node_id: HashMap<String, Entry>,
    #[serde(default)]
    #[allow(dead_code)]
    pub options: Options,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Bare-specifier module path, e.g. `"@/components/ds/Switch"`.
    pub module: String,
    /// `"default"` for default exports, or the named export string.
    pub export: String,
    /// Local identifier to bind under. Defaults to the export name (or, for
    /// default exports, the sanitized mapping key).
    #[serde(default)]
    pub alias: Option<String>,
    /// Reserved — preserves the inlined children inside the replacement.
    /// Parsed but not yet honored; future work.
    #[serde(default)]
    #[allow(dead_code)]
    pub preserve_children: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Options {
    /// Reserved — pass the outer `className` through to the replacement
    /// (`<Switch className="..." />`). Parsed but not yet honored.
    #[serde(default)]
    pub pass_class_name: bool,
}

impl InstanceMap {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading instance map {}", path.display()))?;
        let map: InstanceMap = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing instance map {}", path.display()))?;
        Ok(map)
    }

    /// Decide the local identifier the import should bind under. The data-name
    /// can contain characters that aren't valid JS identifiers (slashes,
    /// non-ASCII, hyphens), so default-exports require an alias derived from
    /// either the mapping's explicit `alias` field or a sanitized data-name.
    pub fn resolve_binding(entry: &Entry, data_name: &str) -> String {
        if let Some(alias) = &entry.alias {
            return alias.clone();
        }
        if entry.export != "default" {
            return entry.export.clone();
        }
        sanitize_identifier(data_name)
    }
}

/// Turn an arbitrary string into a PascalCase-ish JS identifier.
/// `system/person_add` → `SystemPersonAdd`; non-ASCII names collapse to `Comp`.
pub fn sanitize_identifier(name: &str) -> String {
    let mut out = String::new();
    let mut next_upper = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if next_upper {
                out.extend(ch.to_uppercase());
                next_upper = false;
            } else {
                out.push(ch);
            }
        } else if ch == '_' || ch == '-' || ch == '/' || ch.is_whitespace() {
            next_upper = true;
        }
        // non-ASCII chars are dropped; the caller should usually set an alias.
    }
    if out.is_empty()
        || out
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_identifier_basic() {
        assert_eq!(sanitize_identifier("Switch"), "Switch");
        assert_eq!(sanitize_identifier("system/person_add"), "SystemPersonAdd");
        assert_eq!(sanitize_identifier("Frame 9468"), "Frame9468");
        assert_eq!(sanitize_identifier("1"), "_1");
    }

    #[test]
    fn parses_full_map() {
        let json = r#"{
          "mappings": {
            "Switch": { "module": "@/x", "export": "Switch" },
            "DropdownBox": { "module": "@/y", "export": "Dropdown", "alias": "Dropdown" }
          },
          "byNodeId": {
            "1:2": { "module": "@/z", "export": "default" }
          },
          "options": { "passClassName": true }
        }"#;
        let map: InstanceMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.mappings.len(), 2);
        assert_eq!(map.by_node_id.len(), 1);
        assert!(map.options.pass_class_name);
        assert_eq!(
            map.mappings["DropdownBox"].alias.as_deref(),
            Some("Dropdown")
        );
    }

    #[test]
    fn resolve_binding_uses_alias_or_export_or_sanitized() {
        let e = Entry {
            module: "x".into(),
            export: "default".into(),
            alias: None,
            preserve_children: false,
        };
        assert_eq!(
            InstanceMap::resolve_binding(&e, "system/person_add"),
            "SystemPersonAdd"
        );

        let e2 = Entry {
            module: "x".into(),
            export: "Switch".into(),
            alias: None,
            preserve_children: false,
        };
        assert_eq!(InstanceMap::resolve_binding(&e2, "Switch"), "Switch");

        let e3 = Entry {
            module: "x".into(),
            export: "default".into(),
            alias: Some("MySwitch".into()),
            preserve_children: false,
        };
        assert_eq!(InstanceMap::resolve_binding(&e3, "anything"), "MySwitch");
    }
}
