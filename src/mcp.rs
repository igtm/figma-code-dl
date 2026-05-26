//! MCP (Model Context Protocol) client over Streamable HTTP transport, aimed
//! at the local Figma Dev Mode MCP server (`http://127.0.0.1:3845/mcp`).
//!
//! Each MCP call is a single POST request. The server replies either with
//! `application/json` (a single JSON-RPC response) or `text/event-stream`
//! (an SSE stream carrying the JSON-RPC response — used for larger payloads
//! like `get_design_context`).
//!
//! `initialize` produces an `Mcp-Session-Id` response header that we echo
//! back on subsequent requests, along with `MCP-Protocol-Version` derived
//! from the negotiated protocol version.

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

use crate::extract::ContentBlock;

const PROTOCOL_VERSION: &str = "2025-03-26";

pub struct McpClient {
    http: Client,
    endpoint: String,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    next_id: AtomicU64,
    /// Total `call_tool` attempts when the server reports
    /// "No node could be found ..." (a typical signal that Figma desktop
    /// has not yet switched to the right tab). `1` means no retry.
    inactive_retry_attempts: u32,
    inactive_retry_interval_ms: u64,
}

impl McpClient {
    pub fn new(endpoint: String) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("building reqwest client");
        Self {
            http,
            endpoint,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(None),
            next_id: AtomicU64::new(1),
            inactive_retry_attempts: 1,
            inactive_retry_interval_ms: 300,
        }
    }

    /// Retry every `call_tool` up to `attempts` times whenever the server
    /// responds with "No node could be found", waiting `interval_ms` between
    /// attempts. Use `attempts = 1` to disable retry.
    pub fn with_inactive_retry(mut self, attempts: u32, interval_ms: u64) -> Self {
        self.inactive_retry_attempts = attempts.max(1);
        self.inactive_retry_interval_ms = interval_ms;
        self
    }

    pub async fn initialize(&self) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "figma-code-dl",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        });

        let resp = self.post(body).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("initialize HTTP {status}: {text}");
        }
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            *self.session_id.lock().await = Some(sid);
        }

        let result = read_jsonrpc_result(resp, id).await?;
        if let Some(v) = result.get("protocolVersion").and_then(|v| v.as_str()) {
            *self.protocol_version.lock().await = Some(v.to_string());
        }

        // Acknowledge.
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let resp = self.post(notif).await?;
        if !resp.status().is_success() {
            bail!("initialized notification rejected: HTTP {}", resp.status());
        }
        let _ = resp.bytes().await;
        Ok(())
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Vec<ContentBlock>> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=self.inactive_retry_attempts {
            match self.call_tool_once(name, args.clone()).await {
                Ok(blocks) => {
                    if attempt > 1 {
                        eprintln!(
                            "→ {name} settled on attempt {} (waited ~{}ms total)",
                            attempt,
                            (attempt - 1) as u64 * self.inactive_retry_interval_ms
                        );
                    }
                    return Ok(blocks);
                }
                Err(e) => {
                    let retryable = e.to_string().contains("No node could be found");
                    if !retryable || attempt == self.inactive_retry_attempts {
                        return Err(e);
                    }
                    if attempt == 1 {
                        eprintln!(
                            "→ active tab not ready for {name}; retrying (up to {} attempts × {}ms)",
                            self.inactive_retry_attempts, self.inactive_retry_interval_ms
                        );
                    }
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(self.inactive_retry_interval_ms))
                        .await;
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow!("retry loop for tools/call {name} exited without an error")))
    }

    async fn call_tool_once(&self, name: &str, args: Value) -> Result<Vec<ContentBlock>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        let resp = self.post(body).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("tools/call {name} HTTP {status}: {text}");
        }
        let result = read_jsonrpc_result(resp, id).await?;
        let content = result
            .get("content")
            .ok_or_else(|| anyhow!("tools/call {name} result missing `content`: {result}"))?;
        let blocks: Vec<ContentBlock> =
            serde_json::from_value(content.clone()).context("deserializing content blocks")?;
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            let detail = blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            bail!("tools/call {name} reported isError=true:\n{detail}");
        }
        Ok(blocks)
    }

    async fn post(&self, body: Value) -> Result<reqwest::Response> {
        let mut req = self
            .http
            .post(&self.endpoint)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(sid) = self.session_id.lock().await.as_ref() {
            req = req.header("Mcp-Session-Id", sid.clone());
        }
        if let Some(ver) = self.protocol_version.lock().await.as_ref() {
            req = req.header("MCP-Protocol-Version", ver.clone());
        }
        req.send().await.with_context(|| {
            format!(
                "POST {} — is the Figma desktop app running with \
                 `Preferences → Enable Dev Mode MCP server` turned on?",
                self.endpoint
            )
        })
    }
}

async fn read_jsonrpc_result(resp: reqwest::Response, expected_id: u64) -> Result<Value> {
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if ct.starts_with("application/json") {
        let value: Value = resp
            .json()
            .await
            .context("parsing JSON-RPC response body")?;
        return extract_result(value, expected_id);
    }

    if ct.starts_with("text/event-stream") {
        return read_sse_until_response(resp, expected_id).await;
    }

    let text = resp.text().await.unwrap_or_default();
    bail!("unexpected MCP response content-type `{ct}`: {text}");
}

fn extract_result(value: Value, expected_id: u64) -> Result<Value> {
    if let Some(err) = value.get("error") {
        bail!("MCP JSON-RPC error: {err}");
    }
    if let Some(id) = value.get("id").and_then(|v| v.as_u64())
        && id != expected_id
    {
        bail!("MCP response id {id} did not match expected {expected_id}");
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("MCP response missing `result`: {value}"))
}

async fn read_sse_until_response(resp: reqwest::Response, expected_id: u64) -> Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        while let Some(event) = take_event(&mut buf) {
            if event.event.as_deref().unwrap_or("message") != "message" {
                continue;
            }
            let value: Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if value.get("id").and_then(|v| v.as_u64()) != Some(expected_id) {
                continue;
            }
            return extract_result(value, expected_id);
        }
        match stream.next().await {
            Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
            Some(Err(e)) => return Err(anyhow::Error::new(e).context("SSE stream error")),
            None => bail!("SSE stream ended before id {expected_id} response"),
        }
    }
}

#[derive(Debug)]
struct SseEvent {
    event: Option<String>,
    data: String,
}

fn take_event(buf: &mut Vec<u8>) -> Option<SseEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    let (boundary_idx, boundary_len) = if let Some(i) = s.find("\n\n") {
        (i, 2)
    } else if let Some(i) = s.find("\r\n\r\n") {
        (i, 4)
    } else {
        return None;
    };

    let event_text = s[..boundary_idx].to_string();
    buf.drain(..boundary_idx + boundary_len);

    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    for line in event_text.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(rest.to_string());
        }
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_event_parses_message() {
        let mut buf = b"event: message\ndata: {\"id\":1}\n\n".to_vec();
        let ev = take_event(&mut buf).unwrap();
        assert_eq!(ev.event.as_deref(), Some("message"));
        assert_eq!(ev.data, "{\"id\":1}");
        assert!(buf.is_empty());
    }

    #[test]
    fn take_event_joins_multiline_data() {
        let mut buf = b"event: message\ndata: a\ndata: b\n\n".to_vec();
        let ev = take_event(&mut buf).unwrap();
        assert_eq!(ev.data, "a\nb");
    }

    #[test]
    fn take_event_none_when_incomplete() {
        let raw = b"event: message\ndata: partial";
        let mut buf = raw.to_vec();
        assert!(take_event(&mut buf).is_none());
        assert_eq!(buf.len(), raw.len());
    }

    #[test]
    fn extract_result_errors_on_jsonrpc_error() {
        let v = serde_json::json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"bad"}});
        assert!(extract_result(v, 1).is_err());
    }

    #[test]
    fn extract_result_ok() {
        let v = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"x":1}});
        let r = extract_result(v, 1).unwrap();
        assert_eq!(r["x"], 1);
    }
}
