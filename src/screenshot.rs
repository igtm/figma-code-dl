//! Capture a PNG screenshot of a Figma node via the local Dev Mode MCP's
//! `get_screenshot` tool.
//!
//! Unlike `get_design_context` (which rejects section nodes and asks the
//! caller to recurse into a child frame), `get_screenshot` works on any
//! node type — sections included — and returns a single PNG image of what
//! Figma would render. Use it as a low-cost preview alongside the TSX
//! extraction, or as a standalone "show me what this URL points at"
//! operation when the URL turns out to be a section.
//!
//! Response shape: the MCP returns a `Vec<ContentBlock>` containing one block
//! with `type == "image"`, `mimeType == "image/png"`, and `data` being the
//! base64-encoded PNG.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use std::path::Path;

use crate::mcp::McpClient;

#[derive(Debug, Default)]
pub struct ScreenshotReport {
    pub bytes_written: u64,
    pub mime_type: String,
}

pub async fn capture(
    client: &McpClient,
    node_id: &str,
    contents_only: bool,
    out_path: &Path,
) -> Result<ScreenshotReport> {
    let blocks = client
        .call_tool(
            "get_screenshot",
            serde_json::json!({
                "nodeId": node_id,
                "contentsOnly": contents_only,
            }),
        )
        .await
        .context("MCP get_screenshot")?;

    let image = blocks
        .iter()
        .find(|b| b.block_type == "image")
        .ok_or_else(|| {
            anyhow!(
                "MCP `get_screenshot` returned {} content block(s) but none of type=\"image\"; \
                 expected a base64 PNG payload",
                blocks.len()
            )
        })?;

    let data = image.data.as_deref().ok_or_else(|| {
        anyhow!("image content block from `get_screenshot` is missing the `data` field")
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("base64-decoding the screenshot payload")?;

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    std::fs::write(out_path, &bytes)
        .with_context(|| format!("writing screenshot to {}", out_path.display()))?;

    Ok(ScreenshotReport {
        bytes_written: bytes.len() as u64,
        mime_type: image
            .mime_type
            .clone()
            .unwrap_or_else(|| "image/png".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ContentBlock;

    /// Helper: build a fake set of content blocks like what get_screenshot
    /// returns, and run only the decode/write half of the pipeline.
    fn decode_and_write(blocks: &[ContentBlock], out: &Path) -> Result<ScreenshotReport> {
        let image = blocks
            .iter()
            .find(|b| b.block_type == "image")
            .ok_or_else(|| anyhow!("no image block"))?;
        let data = image
            .data
            .as_deref()
            .ok_or_else(|| anyhow!("missing data"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        std::fs::write(out, &bytes)?;
        Ok(ScreenshotReport {
            bytes_written: bytes.len() as u64,
            mime_type: image
                .mime_type
                .clone()
                .unwrap_or_else(|| "image/png".into()),
        })
    }

    #[test]
    fn decodes_image_block_to_disk() {
        // 1x1 transparent PNG, base64-encoded.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let blocks = vec![
            ContentBlock {
                block_type: "text".into(),
                text: "ignored".into(),
                data: None,
                mime_type: None,
            },
            ContentBlock {
                block_type: "image".into(),
                text: String::new(),
                data: Some(png_b64.into()),
                mime_type: Some("image/png".into()),
            },
        ];
        let tmp = std::env::temp_dir().join(format!(
            "figma-code-dl-screenshot-{}.png",
            std::process::id()
        ));
        let report = decode_and_write(&blocks, &tmp).unwrap();
        assert_eq!(report.mime_type, "image/png");
        assert!(report.bytes_written > 0);
        let written = std::fs::read(&tmp).unwrap();
        assert!(written.starts_with(b"\x89PNG\r\n\x1a\n"));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn errors_when_no_image_block() {
        let blocks = vec![ContentBlock {
            block_type: "text".into(),
            text: "only text".into(),
            data: None,
            mime_type: None,
        }];
        let err = decode_and_write(&blocks, Path::new("/tmp/unused.png")).unwrap_err();
        assert!(err.to_string().contains("no image block"));
    }

    #[test]
    fn errors_when_image_block_missing_data() {
        let blocks = vec![ContentBlock {
            block_type: "image".into(),
            text: String::new(),
            data: None,
            mime_type: Some("image/png".into()),
        }];
        let err = decode_and_write(&blocks, Path::new("/tmp/unused.png")).unwrap_err();
        assert!(err.to_string().contains("missing data"));
    }
}
