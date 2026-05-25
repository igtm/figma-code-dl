//! Shared import-emission helper used by both the instance-replace pass
//! (`replace.rs`) and the icon-component pass (`icons.rs`). Coalesces multiple
//! imports from the same module into a single `import { A, B } from "...";`
//! line and supports default + named on the same module.

use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct Imports {
    by_module: BTreeMap<String, ImportSet>,
}

#[derive(Default, Debug)]
struct ImportSet {
    default: Option<String>,
    /// export name → local binding name
    named: BTreeMap<String, String>,
}

impl Imports {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an import. `export == "default"` means a default import; the first
    /// default for a module wins (subsequent ones are ignored to avoid binding
    /// the same default twice).
    pub fn add(&mut self, module: &str, export: &str, local: &str) {
        let set = self.by_module.entry(module.to_string()).or_default();
        if export == "default" {
            if set.default.is_none() {
                set.default = Some(local.to_string());
            }
        } else {
            set.named.insert(export.to_string(), local.to_string());
        }
    }

    /// Merge another import set into this one (other's entries are added on
    /// top of self's, with default-import collision rules unchanged).
    pub fn merge(&mut self, other: Imports) {
        for (module, set) in other.by_module {
            let target = self.by_module.entry(module).or_default();
            if target.default.is_none() {
                target.default = set.default;
            }
            for (export, local) in set.named {
                target.named.entry(export).or_insert(local);
            }
        }
    }

    /// Render as a sequence of `import ... from "...";` lines followed by a
    /// blank line. Returns the empty string when there are no imports.
    pub fn render(&self) -> String {
        if self.by_module.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for (module, set) in &self.by_module {
            let mut parts: Vec<String> = Vec::new();
            if let Some(d) = &set.default {
                parts.push(d.clone());
            }
            if !set.named.is_empty() {
                let named: Vec<String> = set
                    .named
                    .iter()
                    .map(|(export, local)| {
                        if export == local {
                            export.clone()
                        } else {
                            format!("{export} as {local}")
                        }
                    })
                    .collect();
                parts.push(format!("{{ {} }}", named.join(", ")));
            }
            out.push_str(&format!("import {} from \"{module}\";\n", parts.join(", ")));
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_per_module() {
        let mut imps = Imports::new();
        imps.add("@/x", "A", "A");
        imps.add("@/x", "B", "B");
        imps.add("@/y", "default", "Y");
        let rendered = imps.render();
        assert!(rendered.contains(r#"import { A, B } from "@/x";"#));
        assert!(rendered.contains(r#"import Y from "@/y";"#));
    }

    #[test]
    fn merge_unions() {
        let mut a = Imports::new();
        a.add("@/x", "A", "A");
        let mut b = Imports::new();
        b.add("@/x", "B", "B");
        b.add("@/y", "default", "Y");
        a.merge(b);
        let r = a.render();
        assert!(r.contains(r#"import { A, B } from "@/x";"#));
        assert!(r.contains(r#"import Y from "@/y";"#));
    }

    #[test]
    fn default_collision_first_wins() {
        let mut imps = Imports::new();
        imps.add("@/x", "default", "First");
        imps.add("@/x", "default", "Second");
        let r = imps.render();
        assert!(r.contains(r#"import First from "@/x";"#));
        assert!(!r.contains("Second"));
    }

    #[test]
    fn empty_renders_empty() {
        assert!(Imports::new().render().is_empty());
    }

    #[test]
    fn aliased_named_export() {
        let mut imps = Imports::new();
        imps.add("@/x", "Switch", "DSSwitch");
        let r = imps.render();
        assert!(r.contains(r#"import { Switch as DSSwitch } from "@/x";"#));
    }
}
