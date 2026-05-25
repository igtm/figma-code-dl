//! Asset (image / SVG) downloader.
//!
//! Figma MCP aliases each referenced asset via a `const imgFoo = "<url>"`
//! declaration at the top of the TSX. Two URL flavours occur in the wild:
//!
//! - `http://127.0.0.1:3845/assets/<hash>.<ext>` — local Dev Mode MCP. The
//!   extension is in the URL and the asset is served by the Figma desktop
//!   app while it's running.
//! - `https://www.figma.com/api/mcp/asset/<uuid>` — cloud MCP. Extensionless;
//!   we sniff the content type. These URLs expire after seven days.
//!
//! `download_and_rewrite` fetches each unique URL and rewrites the const
//! declaration to point at a relative file path.

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

fn const_decl_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^const\s+(img[A-Za-z0-9]+)\s*=\s*"(https?://(?:www\.figma\.com/api/mcp/asset/[a-f0-9-]+|(?:127\.0\.0\.1|localhost):3845/assets/[a-f0-9]+\.[A-Za-z0-9]+))"\s*;"#,
        )
        .unwrap()
    })
}

/// Pull the extension straight out of a Dev Mode asset URL
/// (`.../assets/<hash>.<ext>`). Returns `None` for cloud URLs.
fn ext_from_url(url: &str) -> Option<String> {
    if !(url.starts_with("http://127.0.0.1:3845/assets/")
        || url.starts_with("http://localhost:3845/assets/"))
    {
        return None;
    }
    let last = url.rsplit('/').next()?;
    let dot = last.rfind('.')?;
    let ext = &last[dot + 1..];
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_string())
}

pub struct DownloadReport {
    pub count: usize,
    pub total_bytes: u64,
}

pub async fn download_and_rewrite(
    code: &str,
    out_tsx_path: &Path,
    assets_dir: &Path,
    http: &reqwest::Client,
) -> Result<(String, DownloadReport)> {
    let assets: Vec<(String, String)> = const_decl_re()
        .captures_iter(code)
        .map(|c| {
            (
                c.get(1).unwrap().as_str().to_string(),
                c.get(2).unwrap().as_str().to_string(),
            )
        })
        .collect();

    if assets.is_empty() {
        return Ok((
            code.to_string(),
            DownloadReport {
                count: 0,
                total_bytes: 0,
            },
        ));
    }

    std::fs::create_dir_all(assets_dir)
        .with_context(|| format!("creating {}", assets_dir.display()))?;

    // Deduplicate by URL — multiple const decls may point to the same asset.
    let mut url_to_first_var: HashMap<String, String> = HashMap::new();
    for (var, url) in &assets {
        url_to_first_var
            .entry(url.clone())
            .or_insert_with(|| var.clone());
    }

    // Download all unique URLs in parallel.
    let mut handles = Vec::new();
    for (url, first_var) in url_to_first_var {
        let http = http.clone();
        let assets_dir = assets_dir.to_path_buf();
        handles.push(tokio::spawn(async move {
            download_one(&http, &url, &first_var, &assets_dir).await
        }));
    }

    let mut url_to_path: HashMap<String, PathBuf> = HashMap::new();
    let mut total_bytes: u64 = 0;
    for h in handles {
        let res = h.await.context("asset download task panicked")?;
        let (url, path, size) = res?;
        total_bytes += size;
        url_to_path.insert(url, path);
    }

    // Compute relative paths from the .tsx output directory.
    let out_dir = out_tsx_path.parent().unwrap_or_else(|| Path::new("."));
    let mut url_to_rel: HashMap<String, String> = HashMap::new();
    for (url, abs_path) in &url_to_path {
        let rel = relative_path(out_dir, abs_path);
        let mut rel_str = rel.to_string_lossy().to_string();
        // ESM/Vite/Webpack expect explicit `./` for sibling/descendant paths.
        if !(rel_str.starts_with("./") || rel_str.starts_with("../") || rel_str.starts_with('/')) {
            rel_str = format!("./{rel_str}");
        }
        url_to_rel.insert(url.clone(), rel_str);
    }

    let count = url_to_rel.len();
    let mut new_code = code.to_string();
    for (url, rel) in &url_to_rel {
        new_code = new_code.replace(url, rel);
    }

    Ok((new_code, DownloadReport { count, total_bytes }))
}

async fn download_one(
    http: &reqwest::Client,
    url: &str,
    var_name: &str,
    assets_dir: &Path,
) -> Result<(String, PathBuf, u64)> {
    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP status for {url}"))?;
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading body of {url}"))?;
    // Prefer the URL-embedded extension (Dev Mode style) over Content-Type,
    // since URL form is unambiguous; fall back to header / magic-byte sniffing
    // for the cloud variant.
    let ext = ext_from_url(url)
        .or_else(|| ext_for_content_type(&ct).map(String::from))
        .or_else(|| sniff_ext(&bytes).map(String::from))
        .ok_or_else(|| anyhow!("could not determine extension for {url} (content-type=`{ct}`)"))?;

    let base = kebab_from_const_name(var_name);
    let mut path = assets_dir.join(base);
    path.set_extension(&ext);
    std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;

    Ok((url.to_string(), path, bytes.len() as u64))
}

fn ext_for_content_type(ct: &str) -> Option<&'static str> {
    let main = ct.split(';').next()?.trim();
    Some(match main {
        "image/png" => "png",
        "image/svg+xml" => "svg",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return None,
    })
}

/// Last-resort extension guess from the first few bytes.
fn sniff_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        Some("svg")
    } else {
        None
    }
}

/// `imgFrameInspect1` → `frame-inspect-1`; `imgImage145` → `image-145`.
fn kebab_from_const_name(var: &str) -> String {
    let s = var.strip_prefix("img").unwrap_or(var);
    let mut out = String::new();
    let mut prev_kind: CharKind = CharKind::Start;
    for ch in s.chars() {
        let kind = if ch.is_ascii_uppercase() {
            CharKind::Upper
        } else if ch.is_ascii_lowercase() {
            CharKind::Lower
        } else if ch.is_ascii_digit() {
            CharKind::Digit
        } else {
            CharKind::Other
        };
        let need_dash = matches!(
            (prev_kind, kind),
            (CharKind::Lower, CharKind::Upper)
                | (CharKind::Lower, CharKind::Digit)
                | (CharKind::Digit, CharKind::Upper)
                | (CharKind::Upper, CharKind::Digit)
        );
        if need_dash {
            out.push('-');
        }
        match kind {
            CharKind::Upper => out.extend(ch.to_lowercase()),
            CharKind::Lower | CharKind::Digit => out.push(ch),
            CharKind::Other => {}
            CharKind::Start => unreachable!(),
        }
        prev_kind = kind;
    }
    if out.is_empty() {
        out.push_str("asset");
    }
    out
}

#[derive(Copy, Clone, PartialEq)]
enum CharKind {
    Start,
    Upper,
    Lower,
    Digit,
    Other,
}

/// Compute a path expressing `to` relative to `from_dir`. Does not touch the
/// filesystem (works on logical paths) — important because both arguments may
/// not exist yet when we compute the result.
fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from = absolute(from_dir);
    let to = absolute(to);

    let from_comps: Vec<Component> = normalize(&from).collect();
    let to_comps: Vec<Component> = normalize(&to).collect();

    let mut common = 0;
    while common < from_comps.len()
        && common < to_comps.len()
        && from_comps[common] == to_comps[common]
    {
        common += 1;
    }

    let mut result = PathBuf::new();
    for _ in &from_comps[common..] {
        result.push("..");
    }
    for c in &to_comps[common..] {
        result.push(c.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Yield path components with `.` removed and `..` collapsed against the prior
/// component when possible.
fn normalize(p: &Path) -> impl Iterator<Item = Component<'_>> {
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.last(), Some(Component::Normal(_))) => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn kebab_basics() {
        assert_eq!(kebab_from_const_name("imgImage145"), "image-145");
        assert_eq!(kebab_from_const_name("imgFrameInspect"), "frame-inspect");
        assert_eq!(kebab_from_const_name("imgFrameInspect1"), "frame-inspect-1");
        assert_eq!(
            kebab_from_const_name("imgChevronForward1"),
            "chevron-forward-1"
        );
        assert_eq!(kebab_from_const_name("imgPersonAdd"), "person-add");
    }

    #[test]
    fn ext_from_mime() {
        assert_eq!(ext_for_content_type("image/png"), Some("png"));
        assert_eq!(
            ext_for_content_type("image/svg+xml; charset=utf-8"),
            Some("svg")
        );
        assert_eq!(ext_for_content_type("image/webp"), Some("webp"));
        assert_eq!(ext_for_content_type("text/html"), None);
    }

    #[test]
    fn sniff_png() {
        assert_eq!(sniff_ext(b"\x89PNG\r\n\x1a\nfoo"), Some("png"));
        assert_eq!(sniff_ext(b"<svg ..."), Some("svg"));
        assert_eq!(sniff_ext(b"\xff\xd8\xff"), Some("jpg"));
        assert_eq!(sniff_ext(b"???"), None);
    }

    #[test]
    fn relative_path_sibling() {
        // tsx in src/pages/Foo.tsx → its parent is src/pages/
        // assets in src/pages/assets/img.png
        let from = Path::new("/tmp/src/pages");
        let to = Path::new("/tmp/src/pages/assets/img.png");
        let r = relative_path(from, to);
        assert_eq!(r, PathBuf::from("assets/img.png"));
    }

    #[test]
    fn relative_path_upwards() {
        let from = Path::new("/tmp/src/components/ds/Switch");
        let to = Path::new("/tmp/src/assets/img.png");
        let r = relative_path(from, to);
        assert_eq!(r, PathBuf::from("../../../assets/img.png"));
    }

    #[test]
    fn const_decl_re_matches() {
        let code = r#"
const imgImage145 = "https://www.figma.com/api/mcp/asset/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const imgFoo = "https://www.figma.com/api/mcp/asset/11111111-2222-3333-4444-555555555555";
"#;
        let m: Vec<_> = const_decl_re().captures_iter(code).collect();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].get(1).unwrap().as_str(), "imgImage145");
        assert_eq!(m[1].get(1).unwrap().as_str(), "imgFoo");
    }
}
