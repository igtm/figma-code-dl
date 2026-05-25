//! Real asset-download smoke test. Hits the live Figma asset CDN, so it's
//! `#[ignore]`'d by default — run with:
//!     cargo test --test asset_download -- --ignored

use std::path::PathBuf;

#[path = "../src/assets.rs"]
mod assets;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
#[ignore]
async fn downloads_assets_and_rewrites_urls() {
    let root = project_root();
    let tsx = std::fs::read_to_string(root.join("tmp/02_extracted.tsx")).unwrap();
    let assets_dir = root.join("tmp/assets");
    let _ = std::fs::remove_dir_all(&assets_dir);

    let out_tsx = root.join("tmp/v2-output.tsx");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();

    let (rewritten, report) = assets::download_and_rewrite(&tsx, &out_tsx, &assets_dir, &http)
        .await
        .unwrap();

    eprintln!(
        "downloaded={}, bytes={}, assets_dir={}",
        report.count,
        report.total_bytes,
        assets_dir.display()
    );

    // Sanity: at least a handful of assets came down, and the original URLs
    // are gone from the rewritten code.
    assert!(
        report.count > 10,
        "expected >10 assets, got {}",
        report.count
    );
    assert!(report.total_bytes > 1000);
    assert!(
        !rewritten.contains("https://www.figma.com/api/mcp/asset/"),
        "asset URLs should have been rewritten away"
    );

    // Stash the rewritten TSX for inspection.
    std::fs::write(out_tsx, rewritten).unwrap();

    // Asset directory should have files.
    let entries: Vec<_> = std::fs::read_dir(&assets_dir).unwrap().collect();
    assert_eq!(entries.len(), report.count);
}
