use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::Read;
use std::path::PathBuf;

mod assets;
mod colors;
mod extract;
mod figma_url;
mod icons;
mod imports;
mod instance_map;
mod mcp;
mod replace;
mod screenshot;
mod trim;
mod variables_dump;

#[derive(Parser, Debug)]
#[command(
    name = "figma-code-dl",
    version,
    about = "Pull a Figma node from the local Dev Mode MCP server and produce a clean React+Tailwind .tsx, with inlined-instance replacement and asset handling."
)]
struct Args {
    /// Figma URL. Required unless `--from-json` is given. The fileKey is
    /// ignored (the Dev Mode MCP operates on the active Figma tab), but the
    /// URL is parsed for the nodeId and for the output header.
    url: Option<String>,

    /// Output .tsx file path. Required unless only `--dump-variables` is
    /// being used.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Endpoint of the local Figma MCP server (Streamable HTTP).
    #[arg(long, default_value = "http://127.0.0.1:3845/mcp")]
    mcp_url: String,

    /// Read the MCP `content` blocks from a JSON file (or `-` for stdin)
    /// instead of fetching them. Used when another MCP client did the fetch.
    #[arg(long, conflicts_with = "url")]
    from_json: Option<PathBuf>,

    /// Override the URL recorded in the output header. Defaults to the
    /// `--url` value (or to a placeholder when reading from `--from-json`).
    #[arg(long)]
    source_url: Option<String>,

    /// Path to `.figma/instance-map.json`. When given, JSX elements whose
    /// `data-name` (or `data-node-id`) is in the mapping get replaced with a
    /// `<Component />` reference and the corresponding `import` is injected.
    #[arg(long)]
    map: Option<PathBuf>,

    /// Print a histogram of inlined-instance `data-name` values that no
    /// mapping handled.
    #[arg(long)]
    report_unmapped: bool,

    /// Download referenced Figma assets into this directory and rewrite
    /// their URLs in the output to relative paths. When fetching live, also
    /// passes the absolute path as `dirForAssetWrites` to the MCP server so
    /// Figma writes assets directly.
    #[arg(long)]
    download_assets: Option<PathBuf>,

    /// Save the raw MCP `content` blocks (as JSON) to this path. Useful when
    /// debugging or for showing the before/after of post-processing.
    #[arg(long)]
    dump_raw: Option<PathBuf>,

    /// Enable the trim pass using `.figma/config.toml`. Removes redundant
    /// Tailwind classes (per `[trim]` rules) and listed JSX attributes from
    /// the output to reduce token count when the file is later read by an LLM.
    #[arg(long)]
    trim: bool,

    /// Explicit path to a trim config TOML file. Implies `--trim`.
    #[arg(long)]
    trim_config: Option<PathBuf>,

    /// Enable the icons pass using `.figma/config.toml` (`[icons]` section).
    /// Substitutes `<img src={imgXxx} ... />` with reusable React icon
    /// components (e.g. SVG components) when a matching component exists in
    /// the configured directory or in the explicit overrides.
    #[arg(long)]
    icons: bool,

    /// Explicit path to an icons config TOML file. Implies `--icons`.
    #[arg(long)]
    icons_config: Option<PathBuf>,

    /// Enable the colors pass using `.figma/variables.json`. Replaces bare
    /// `[#XXXXXX]` color tokens with `[var(--name,#XXX)]` references when
    /// the hex exactly matches a variable whose `codeSyntax.WEB` is set in
    /// Figma.
    #[arg(long)]
    colors: bool,

    /// Explicit path to a `.figma/variables.json` file. Implies `--colors`.
    #[arg(long)]
    colors_file: Option<PathBuf>,

    /// Fetch Figma Variables via MCP `get_variable_defs` and write them to
    /// this path. Can be combined with `--out` (does both); when used alone
    /// (without `--out`), only the dump runs.
    #[arg(long)]
    dump_variables: Option<PathBuf>,

    /// Capture a PNG screenshot of the target node via MCP `get_screenshot`
    /// and write it to this path. Works on `section` nodes too (where
    /// `get_design_context` would fail), so this can be a standalone
    /// preview-only run with no `--out`.
    #[arg(long)]
    screenshot: Option<PathBuf>,

    /// Pass `contentsOnly: true` to `get_screenshot` so the node is rendered
    /// in isolation, without anything that visually overlaps it on the
    /// canvas. Default is `false` (canvas-as-seen rendering).
    #[arg(long)]
    screenshot_contents_only: bool,

    /// Skip auto-activating the Figma desktop tab before MCP calls. By
    /// default (macOS only), `figma-code-dl` runs `open -a "Figma" <url>`
    /// before any MCP call so the Dev Mode MCP server sees the right active
    /// tab, even when the user was on a different file. Pass this flag to
    /// turn that off — useful if you've already activated the tab manually
    /// and don't want the focus shuffle.
    #[arg(long)]
    no_activate: bool,

    /// Milliseconds to wait after running `open -a "Figma" <url>` to give
    /// the desktop app time to switch tabs before the MCP call lands.
    /// Default is 800ms.
    #[arg(long, default_value_t = 800)]
    activate_wait_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.out.is_none() && args.dump_variables.is_none() && args.screenshot.is_none() {
        bail!(
            "at least one of `--out <path>`, `--dump-variables <path>`, or `--screenshot <path>` is required"
        );
    }

    let source_url = args.source_url.clone().or_else(|| args.url.clone());
    let target = source_url.as_deref().and_then(|u| figma_url::parse(u).ok());
    if let Some(t) = &target {
        eprintln!("→ nodeId={}", t.node_id);
    }

    // Bring the right Figma tab to the foreground before any MCP call, so the
    // server (which operates on the active tab) finds the node we asked for.
    // No-op when reading from JSON (no live MCP), when `--no-activate` is set,
    // and on non-macOS platforms.
    if let Some(url) = args.url.as_deref()
        && !args.no_activate
        && args.from_json.is_none()
    {
        activate_figma_tab(url, args.activate_wait_ms);
    }

    if let Some(out) = &args.out
        && let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let abs_assets_dir = args
        .download_assets
        .as_ref()
        .map(|p| {
            std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
            p.canonicalize()
                .with_context(|| format!("canonicalizing {}", p.display()))
        })
        .transpose()?;

    // Standalone-capable MCP operations: variable dump and screenshot. Both
    // only need a nodeId from the URL and a single MCP session, and either
    // can be used without `--out` so a one-shot survey is possible.
    let needs_standalone_mcp = args.dump_variables.is_some() || args.screenshot.is_some();
    if needs_standalone_mcp {
        let node_id_for_mcp = target.as_ref().map(|t| t.node_id.clone()).or_else(|| {
            args.url
                .as_deref()
                .and_then(|u| figma_url::parse(u).ok().map(|t| t.node_id))
        });
        let Some(node_id) = node_id_for_mcp else {
            bail!("--dump-variables / --screenshot require <url> so a nodeId can be passed to MCP");
        };
        let client = mcp::McpClient::new(args.mcp_url.clone());
        client.initialize().await.context("MCP initialize")?;
        eprintln!("→ MCP initialized at {}", args.mcp_url);

        if let Some(dump_path) = &args.dump_variables {
            let report = variables_dump::dump(&client, &node_id, dump_path).await?;
            eprintln!(
                "→ dumped {} variable(s) ({} with codeSyntax.WEB) to {}",
                report.variables_total,
                report.variables_with_codesyntax,
                dump_path.display()
            );
            eprintln!("    raw MCP response: {}", report.raw_path.display());
        }

        if let Some(shot_path) = &args.screenshot {
            let report =
                screenshot::capture(&client, &node_id, args.screenshot_contents_only, shot_path)
                    .await?;
            eprintln!(
                "→ screenshot: {} bytes ({}) → {}",
                report.bytes_written,
                report.mime_type,
                shot_path.display()
            );
        }

        if args.out.is_none() {
            return Ok(());
        }
    }

    let out_path = args
        .out
        .as_ref()
        .expect("validated above: --out is present when we reach this point");

    let blocks = if let Some(from) = &args.from_json {
        read_from_file(from)?
    } else {
        let url = args
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("either <url> or `--from-json <path>` is required"))?;
        let node_id = figma_url::parse(url).context("parsing Figma URL")?.node_id;
        fetch_via_mcp(&args.mcp_url, &node_id, abs_assets_dir.as_deref()).await?
    };

    if let Some(p) = &args.dump_raw {
        if let Some(parent) = p.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_vec_pretty(&blocks).context("serializing raw blocks")?;
        std::fs::write(p, &json).with_context(|| format!("writing raw dump to {}", p.display()))?;
        eprintln!("→ raw blocks dumped to {}", p.display());
    }

    let extracted = extract::extract(&blocks)?;

    let map = match &args.map {
        Some(p) => instance_map::InstanceMap::load(p)
            .with_context(|| format!("loading instance map {}", p.display()))?,
        None => instance_map::InstanceMap::default(),
    };
    let processed = replace::process(&extracted.code, &map)?;

    if !processed.report.replaced.is_empty() {
        let total: u32 = processed.report.replaced.values().sum();
        eprintln!(
            "→ replaced {} instance(s) across {} mapping(s)",
            total,
            processed.report.replaced.len()
        );
        for (name, count) in &processed.report.replaced {
            eprintln!("    {count:>4}× {name}");
        }
    }
    if !processed.report.removed_functions.is_empty() {
        eprintln!(
            "→ removed {} server-extracted function declaration(s): {}",
            processed.report.removed_functions.len(),
            processed.report.removed_functions.join(", ")
        );
    }
    if args.report_unmapped && !processed.report.unmapped.is_empty() {
        let mut sorted: Vec<(&String, &u32)> = processed.report.unmapped.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        eprintln!(
            "→ unmapped instances ({} distinct):",
            processed.report.unmapped.len()
        );
        for (name, count) in sorted {
            eprintln!("    {count:>4}× {name}");
        }
    }

    let mut imports = processed.imports;
    let mut body = processed.body;

    // Icons pass (overrides phase): substitute <img src={imgXxx} /> with
    // reusable design-system React components BEFORE asset DL, so the matched
    // URLs aren't downloaded needlessly. The local-SVG branch is no-op here
    // because URLs are still remote (no `.svg` extension on cloud URLs).
    let icons_cfg_path: Option<PathBuf> = match (&args.icons_config, args.icons) {
        (Some(p), _) => Some(p.clone()),
        (None, true) => Some(PathBuf::from(".figma/config.toml")),
        (None, false) => None,
    };
    let icons_cfg = match &icons_cfg_path {
        Some(p) => Some(icons::IconConfig::load(p)?),
        None => None,
    };
    if let Some(cfg) = &icons_cfg {
        let icon_out = icons::process(&body, cfg)?;
        let total: u32 = icon_out.report.replaced.values().sum();
        if total > 0 || !icon_out.report.unmatched.is_empty() {
            eprintln!(
                "→ icons: {} substitution(s) across {} component(s), {} unmatched, −{} const decl(s)",
                total,
                icon_out.report.replaced.len(),
                icon_out.report.unmatched.len(),
                icon_out.report.const_decls_removed
            );
        }
        body = icon_out.body;
        imports.merge(icon_out.imports);
    }

    let mut body_after_assets = if let Some(assets_dir) = &args.download_assets {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("building asset HTTP client")?;
        let (rewritten, report) =
            assets::download_and_rewrite(&body, out_path, assets_dir, &http).await?;
        eprintln!(
            "→ {} asset(s) ({} bytes total) → {}",
            report.count,
            report.total_bytes,
            assets_dir.display()
        );
        rewritten
    } else {
        body
    };

    // Icons pass (local-SVG phase): every remaining `const imgXxx = "./assets/foo.svg"`
    // gets converted to a default import + component substitution. Runs AFTER
    // --download-assets so URLs are local paths.
    if let Some(cfg) = &icons_cfg
        && cfg.local_svg_import
    {
        let mut cfg2 = cfg.clone();
        // Skip override resolution on this second pass — anything still here
        // didn't match an override, so the only relevant branch is local-svg.
        cfg2.overrides.clear();
        let icon_out = icons::process(&body_after_assets, &cfg2)?;
        let total: u32 = icon_out.report.replaced.values().sum();
        if total > 0 {
            eprintln!(
                "→ icons (local svg): {} substitution(s) across {} component(s), −{} const decl(s)",
                total,
                icon_out.report.replaced.len(),
                icon_out.report.const_decls_removed
            );
        }
        body_after_assets = icon_out.body;
        imports.merge(icon_out.imports);
    }

    // Colors pass: rewrite `[#XXXXXX]` -> `[var(--name,#XXX)]` per
    // .figma/variables.json. Run AFTER assets so URL rewrites are done, but
    // BEFORE trim so trim only sees the final class shape.
    let colors_path: Option<PathBuf> = match (&args.colors_file, args.colors) {
        (Some(p), _) => Some(p.clone()),
        (None, true) => Some(PathBuf::from(".figma/variables.json")),
        (None, false) => None,
    };
    if let Some(path) = colors_path {
        let map = colors::ColorMap::load(&path)?;
        let (rewritten, report) = colors::process(&body_after_assets, &map);
        if report.substitutions > 0 {
            eprintln!(
                "→ colors: {} substitution(s) across {} distinct hex(es) (from {} mapped variable(s))",
                report.substitutions,
                report.by_hex.len(),
                map.len()
            );
        } else if !map.is_empty() {
            eprintln!(
                "→ colors: 0 substitution(s) ({} variable(s) in {} but no [#XXX] match in output)",
                map.len(),
                path.display()
            );
        }
        body_after_assets = rewritten;
    }

    let trim_cfg_path: Option<PathBuf> = match (&args.trim_config, args.trim) {
        (Some(p), _) => Some(p.clone()),
        (None, true) => Some(PathBuf::from(".figma/config.toml")),
        (None, false) => None,
    };
    if let Some(path) = trim_cfg_path {
        let cfg = trim::TrimConfig::load(&path)?;
        let (trimmed, report) = trim::trim(&body_after_assets, &cfg);
        let saved_bytes = report.bytes_before.saturating_sub(report.bytes_after);
        let pct = if report.bytes_before > 0 {
            (saved_bytes as f64) / (report.bytes_before as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "→ trim: −{} class(es), −{} attribute(s), −{} empty className(s); {} → {} bytes (−{:.1}%)",
            report.classes_removed,
            report.attributes_removed,
            report.classname_attrs_dropped,
            report.bytes_before,
            report.bytes_after,
            pct
        );
        body_after_assets = trimmed;
    }

    let header = extract::build_header(
        source_url
            .as_deref()
            .unwrap_or("(provided via --from-json)"),
        target
            .as_ref()
            .map(|t| t.file_key.as_str())
            .unwrap_or("(unknown)"),
        target
            .as_ref()
            .map(|t| t.node_id.as_str())
            .unwrap_or("(unknown)"),
        extracted.styles_digest.as_deref(),
    );

    let imports_rendered = imports.render();
    let imports_block = if imports_rendered.is_empty() {
        String::new()
    } else {
        format!("{}{}", AUTO_IMPORT_NOTE, imports_rendered)
    };
    let body_with_asset_note = inject_asset_const_note(&body_after_assets);
    let output = format!("{header}{}{}", imports_block, body_with_asset_note);

    std::fs::write(out_path, &output).with_context(|| format!("writing {}", out_path.display()))?;
    eprintln!("→ wrote {} ({} bytes)", out_path.display(), output.len());
    Ok(())
}

/// Warning prepended above the `import ... from ...` block. The import paths
/// are best-guesses derived from Figma names; reviewers (human or LLM) should
/// look for existing equivalents in the codebase first.
const AUTO_IMPORT_NOTE: &str = "\
// NOTE: figma-code-dl auto-generated the imports below from Figma layer /
// asset names. The component paths are best-guesses — before relying on
// them, search this codebase for existing components that cover the same
// thing and re-point the import. Local-SVG defaults (`./assets/*.svg`)
// may also be replaceable by a curated icon already in the repo.
//
";

/// Warning prepended above the `const imgXxx = \"./assets/...\";` block.
const AUTO_ASSET_NOTE: &str = "\
// NOTE: The asset files below were downloaded directly from Figma. They
// may duplicate assets already in this codebase — search for an existing
// image / SVG / icon before keeping these (especially raster images that
// may already exist in optimized form).
//
";

/// Find the first `const imgXxx = ...` line in `body` and insert
/// `AUTO_ASSET_NOTE` immediately above it. No-op when no such const exists.
fn inject_asset_const_note(body: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^const\s+img[A-Za-z0-9_]+\s*=").unwrap());
    match re.find(body) {
        Some(m) => {
            let mut out = String::with_capacity(body.len() + AUTO_ASSET_NOTE.len());
            out.push_str(&body[..m.start()]);
            out.push_str(AUTO_ASSET_NOTE);
            out.push_str(&body[m.start()..]);
            out
        }
        None => body.to_string(),
    }
}

/// Tell Figma Desktop to bring the file at `url` to the foreground. macOS
/// only — `open -a "Figma" <url>` switches the desktop app to that tab. On
/// other platforms we skip silently and rely on the user having already
/// activated the right tab.
///
/// Failures are reported but non-fatal: if the desktop app isn't installed,
/// isn't running, or returns non-zero, the MCP call will fail loudly anyway
/// with a more useful error, so we just continue.
fn activate_figma_tab(url: &str, wait_ms: u64) {
    if !cfg!(target_os = "macos") {
        return;
    }
    match std::process::Command::new("open")
        .args(["-a", "Figma", url])
        .status()
    {
        Ok(status) if status.success() => {
            eprintln!("→ activated Figma tab ({} ms wait)", wait_ms);
            if wait_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            }
        }
        Ok(status) => {
            eprintln!(
                "WARN: `open -a Figma <url>` exited with {status}; continuing — \
                 make sure Figma Desktop has the target file active"
            );
        }
        Err(e) => {
            eprintln!(
                "WARN: failed to run `open -a Figma <url>` ({e}); continuing — \
                 make sure Figma Desktop has the target file active"
            );
        }
    }
}

fn read_from_file(path: &std::path::Path) -> Result<Vec<extract::ContentBlock>> {
    let bytes = if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        buf
    } else {
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
    };
    // Accept two shapes:
    //   1. Raw `Vec<ContentBlock>` (our own output, or `content` field copied out)
    //   2. Full MCP `ToolResult` envelope: `{ "content": [ ... ] }`
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing JSON from {}", path.display()))?;
    let blocks: Vec<extract::ContentBlock> = if value.is_array() {
        serde_json::from_value(value)?
    } else if let Some(content) = value.get("content").cloned() {
        serde_json::from_value(content)?
    } else {
        bail!(
            "JSON from {} is neither an array of ContentBlock nor a `{{ \"content\": [...] }}` envelope",
            path.display()
        );
    };
    eprintln!(
        "→ loaded {} content block(s) from {}",
        blocks.len(),
        path.display()
    );
    Ok(blocks)
}

async fn fetch_via_mcp(
    endpoint: &str,
    node_id: &str,
    assets_dir: Option<&std::path::Path>,
) -> Result<Vec<extract::ContentBlock>> {
    let client = mcp::McpClient::new(endpoint.to_string());
    client.initialize().await.context("MCP initialize")?;
    eprintln!("→ MCP initialized at {endpoint}");

    let mut tool_args = serde_json::json!({
        "nodeId": node_id,
        "forceCode": true,
        "clientFrameworks": "react",
        "clientLanguages": "typescript",
    });
    if let Some(dir) = assets_dir {
        tool_args["dirForAssetWrites"] =
            serde_json::Value::String(dir.to_string_lossy().to_string());
    }

    let blocks = client.call_tool("get_design_context", tool_args).await?;
    eprintln!("→ received {} content block(s)", blocks.len());
    Ok(blocks)
}
