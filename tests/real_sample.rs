//! End-to-end smoke test against the captured sample TSX in `tmp/02_extracted.tsx`.
//!
//! This is not a hermetic unit test — it depends on the captured fixture
//! produced earlier in development. It serves two purposes:
//!   1. Catches regressions in the replacement logic against real Figma output.
//!   2. Writes the post-replacement TSX to `tmp/v1-output.tsx` so the result
//!      can be eyeballed.
//!
//! Skips silently if the fixture or sample map is missing.

use std::path::PathBuf;

#[path = "../src/imports.rs"]
mod imports;
#[path = "../src/instance_map.rs"]
mod instance_map;
#[path = "../src/replace.rs"]
mod replace;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn real_sample_replacement_smoke() {
    let root = project_root();
    let tsx_path = root.join("tmp/02_extracted.tsx");
    let map_path = root.join("tmp/sample-instance-map.json");
    if !tsx_path.exists() || !map_path.exists() {
        eprintln!("SKIP: required fixtures not present");
        return;
    }

    let code = std::fs::read_to_string(&tsx_path).unwrap();
    // Strip the leading `// Auto-extracted ...` header lines so we operate on
    // pure TSX (matches what mcp.rs hands to replace.rs in production).
    let body_start = code
        .find("\nconst img")
        .or_else(|| code.find("\nfunction "))
        .or_else(|| code.find("\nexport "))
        .map(|i| i + 1)
        .unwrap_or(0);
    let tsx = &code[body_start..];

    let map = instance_map::InstanceMap::load(&map_path).unwrap();
    let out = replace::process(tsx, &map).expect("process should succeed");

    // Expectations on the actual sample:
    //   - 2 Switch instances are inlined and should be replaced.
    //   - 2 DropdownBox instances; 9 `list` instances; both should hit.
    //   - The server-extracted `function Button(...)` should be removed.
    assert_eq!(out.report.replaced.get("Switch"), Some(&2));
    assert_eq!(out.report.replaced.get("DropdownBox"), Some(&2));
    assert_eq!(out.report.replaced.get("list"), Some(&9));
    assert!(
        out.report.removed_functions.contains(&"Button".to_string()),
        "expected server-extracted Button function to be removed; removed={:?}",
        out.report.removed_functions
    );

    // Body sanity.
    assert!(
        !out.body.contains("function Button"),
        "function Button should be gone"
    );
    assert!(out.body.contains("<Switch />"));
    assert!(out.body.contains("<Dropdown />"));
    assert!(out.body.contains("<ListRow />"));

    // Imports sanity.
    let rendered = out.imports.render();
    assert!(rendered.contains(r#"import Button from "@/components/ds/Button";"#));
    assert!(
        rendered.contains(r#"import { Dropdown as Dropdown } from"#)
            || rendered.contains(r#"import { Dropdown } from "@/components/ds/Dropdown";"#)
    );
    assert!(rendered.contains(r#"import { Switch } from "@/components/ds/Switch";"#));

    // Write the post-replacement file out for inspection.
    let header = "// v1 instance-replacement smoke test output\n\
                  // Source: tmp/02_extracted.tsx + tmp/sample-instance-map.json\n\n";
    let final_out = format!("{header}{}{}", rendered, out.body);
    std::fs::write(root.join("tmp/v1-output.tsx"), final_out).unwrap();

    eprintln!(
        "v1 sample run: replaced={:?}, removed_fns={:?}, unmapped_distinct={}",
        out.report.replaced,
        out.report.removed_functions,
        out.report.unmapped.len()
    );
}
