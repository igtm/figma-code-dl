//! End-to-end-ish test for the trim pass.

#[path = "../src/trim.rs"]
mod trim;

use trim::{TrimConfig, trim};

fn realistic_snippet() -> String {
    r##"<div className="bg-white content-stretch flex items-start relative size-full" data-node-id="1:2" data-name="Page">
  <div className="bg-[var(--color\/semantic\/background\/green,#def4f2)] content-stretch flex items-center pb-[24px] pt-[16px] px-[16px] relative shrink-0" data-node-id="1523:5825" data-name="Navi">
    <div className="relative shrink-0 size-[40px]" data-node-id="I1:1" data-name="symbol_pattern">
      <img alt="" className="absolute block inset-0 max-w-none size-full" src={imgSymbolPattern} />
    </div>
    <div className="absolute inset-[12.5%] mask-alpha mask-intersect mask-no-clip mask-no-repeat mask-position-[-3px_-3px] mask-size-[24px_24px]" data-node-id="I1:2" style={{ maskImage: `url('${imgFrameInspect}')` }} data-name="frame_inspect">
      <img alt="" className="absolute block inset-0 max-w-none size-full" src={imgFrameInspect1} />
    </div>
  </div>
</div>"##
        .to_string()
}

fn realistic_cfg() -> TrimConfig {
    let raw = r#"
[trim]
exclude_exact = [
  "relative",
  "block",
  "shrink-0",
  "content-stretch",
  "max-w-none",
  "size-full",
  "absolute",
]
exclude_prefixes = ["inset-", "mask-"]
drop_empty_classname = true
strip_attributes = ["data-node-id", "data-name"]
"#;
    #[derive(serde::Deserialize, Default)]
    struct Wrap {
        #[serde(default)]
        trim: TrimConfig,
    }
    toml::from_str::<Wrap>(raw).unwrap().trim
}

#[test]
fn trims_realistic_snippet() {
    let src = realistic_snippet();
    let cfg = realistic_cfg();
    let (out, report) = trim(&src, &cfg);

    for needle in [
        "content-stretch",
        "shrink-0",
        " relative",
        "max-w-none",
        "size-full",
        "inset-[12.5%]",
        "mask-alpha",
        "mask-no-clip",
        "data-node-id",
        "data-name",
    ] {
        assert!(
            !out.contains(needle),
            "expected `{}` trimmed; got:\n{}",
            needle,
            out
        );
    }

    for needle in [
        "className=\"bg-white",
        "bg-[var(--color\\/semantic\\/background\\/green,#def4f2)]",
        "items-center",
        "pb-[24px]",
        "src={imgFrameInspect1}",
    ] {
        assert!(
            out.contains(needle),
            "expected `{}` to remain; got:\n{}",
            needle,
            out
        );
    }

    assert!(report.bytes_after < report.bytes_before);
    let pct = (report.bytes_before - report.bytes_after) as f64 / report.bytes_before as f64;
    assert!(
        pct > 0.25,
        "expected ≥25% byte reduction, got {:.1}%",
        pct * 100.0
    );
    assert!(report.classes_removed > 0);
    assert!(report.attributes_removed > 0);
}

#[test]
fn loads_config_from_disk() {
    let tmp = std::env::temp_dir().join(format!(
        "figma-code-dl-trim-{}.toml",
        std::process::id()
    ));
    std::fs::write(
        &tmp,
        r#"
[trim]
exclude_exact = ["relative"]
exclude_prefixes = []
drop_empty_classname = true
strip_attributes = []
"#,
    )
    .unwrap();
    let cfg = TrimConfig::load(&tmp).unwrap();
    assert_eq!(cfg.exclude_exact, vec!["relative".to_string()]);
    assert!(cfg.drop_empty_classname);
    let _ = std::fs::remove_file(&tmp);
}
