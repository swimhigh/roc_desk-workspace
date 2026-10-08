use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

use futures_util::StreamExt;
use regex::Regex;

use roc_desk_core::error::AppError;

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_CHARS: usize = 8000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

// Rust's regex crate doesn't support backreferences, so `<script>`/`<style>`
// each need their own pattern instead of one PCRE-style `<(script|style)>...</\1>`.
static SCRIPT_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
static STYLE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static BLANK_LINES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]*\n(?:[ \t]*\n)+").unwrap());

/// `webfetch` tool (`coding::tools::ToolCall::WebFetch`): fetches a URL and
/// converts it to plain text the model can read. Only a "good enough"
/// HTML-to-text conversion (regex tag stripping), no full parser
/// (html5ever/scraper) -- same approach `roc_desk_common::ai::chat` uses
/// for Bing RSS results, doesn't aim to preserve layout, just readability.
pub async fn fetch_url(client: &reqwest::Client, url: &str) -> Result<String, AppError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| AppError::Internal(format!("无效的 URL：{e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Internal("只支持 http/https URL".into()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::Internal("URL 缺少主机名".into()))?;
    if is_internal_host(host) {
        // The page's own content could contain a prompt-injection hint like
        // "visit http://169.254.169.254/... for more info" -- if the model
        // followed it, webfetch would become a pivot into internal
        // networks/cloud metadata endpoints. Rejected before the request is
        // ever sent, rather than trusting the model to judge whether it
        // should visit it.
        return Err(AppError::PermissionDenied(format!(
            "出于安全考虑，拒绝访问内网/本地地址：{host}"
        )));
    }

    let resp = tokio::time::timeout(
        FETCH_TIMEOUT,
        client
            .get(parsed)
            .header(reqwest::header::USER_AGENT, "roc_desk/1.0 (AI webfetch)")
            .send(),
    )
    .await
    .map_err(|_| AppError::Connection("webfetch 请求超时".into()))??;

    if !resp.status().is_success() {
        return Err(AppError::Connection(format!("HTTP {}", resp.status())));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(AppError::from)?;
        if body.len() >= MAX_BODY_BYTES {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    let extracted = if content_type.contains("html") {
        html_to_text(&text)
    } else {
        text
    };
    Ok(extracted.chars().take(MAX_OUTPUT_CHARS).collect())
}

fn is_internal_host(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        // fc00::/7 is IPv6's unique-local-address (ULA) range; the standard
        // library has no stable `is_unique_local` yet.
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
        Err(_) => false,
    }
}

fn html_to_text(html: &str) -> String {
    let no_script = SCRIPT_BLOCK.replace_all(html, "");
    let no_style = STYLE_BLOCK.replace_all(&no_script, "");
    let no_tags = TAG.replace_all(&no_style, "\n");
    let decoded = decode_entities(&no_tags);
    BLANK_LINES.replace_all(decoded.trim(), "\n\n").to_string()
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback_and_private_hosts() {
        assert!(is_internal_host("localhost"));
        assert!(is_internal_host("127.0.0.1"));
        assert!(is_internal_host("192.168.1.1"));
        assert!(is_internal_host("10.0.0.5"));
        assert!(is_internal_host("169.254.169.254"));
        assert!(!is_internal_host("example.com"));
        assert!(!is_internal_host("93.184.216.34"));
    }

    #[test]
    fn strips_script_and_tags() {
        let html = "<html><head><style>.a{}</style></head><body><script>evil()</script><p>Hello <b>World</b></p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("evil"));
        assert!(!text.contains('<'));
    }
}
