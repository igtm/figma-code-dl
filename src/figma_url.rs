use anyhow::{Context, Result, bail};
use url::Url;

#[derive(Debug, Clone)]
pub struct FigmaTarget {
    pub file_key: String,
    pub node_id: String,
}

pub fn parse(input: &str) -> Result<FigmaTarget> {
    let url = Url::parse(input).context("invalid URL")?;
    let host = url.host_str().context("URL has no host")?;
    if host != "figma.com" && host != "www.figma.com" {
        bail!("not a Figma URL (host: {host})");
    }

    let segments: Vec<&str> = url
        .path_segments()
        .context("URL has no path")?
        .filter(|s| !s.is_empty())
        .collect();

    let file_key = match segments.as_slice() {
        ["design", _, "branch", branch_key, ..] => (*branch_key).to_string(),
        ["design", file_key, ..] => (*file_key).to_string(),
        ["make", make_key, ..] => (*make_key).to_string(),
        _ => bail!(
            "URL path doesn't look like a Figma design/make URL: /{}",
            segments.join("/")
        ),
    };

    let node_id_raw = url
        .query_pairs()
        .find(|(k, _)| k == "node-id")
        .map(|(_, v)| v.into_owned())
        .context("URL has no node-id query parameter")?;
    let node_id = node_id_raw.replace('-', ":");

    Ok(FigmaTarget { file_key, node_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_design_url() {
        let t = parse("https://www.figma.com/design/AbCdEfGhIjKlMnOpQrStUv/Foo?node-id=1-2&m=dev")
            .unwrap();
        assert_eq!(t.file_key, "AbCdEfGhIjKlMnOpQrStUv");
        assert_eq!(t.node_id, "1:2");
    }

    #[test]
    fn parses_branch_url() {
        let t =
            parse("https://www.figma.com/design/ABCD/branch/BRANCH123/Bar?node-id=1-2").unwrap();
        assert_eq!(t.file_key, "BRANCH123");
        assert_eq!(t.node_id, "1:2");
    }

    #[test]
    fn parses_make_url() {
        let t = parse("https://www.figma.com/make/MAKE123/Baz?node-id=10-20").unwrap();
        assert_eq!(t.file_key, "MAKE123");
        assert_eq!(t.node_id, "10:20");
    }

    #[test]
    fn rejects_non_figma_host() {
        assert!(parse("https://example.com/design/x/y?node-id=1-2").is_err());
    }

    #[test]
    fn requires_node_id() {
        assert!(parse("https://www.figma.com/design/X/Y").is_err());
    }
}
