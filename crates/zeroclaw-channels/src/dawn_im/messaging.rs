//! Message encoding + media download for DawnIM.

use std::time::Duration;

use base64::Engine;

const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;
const FILE_MAX_BYTES: usize = 100 * 1024 * 1024;

const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "bat", "sh", "app", "dmg", "js", "py", "rb", "php", "pl",
];

const SUPPORTED_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// Encode a text content string as a DawnIM type-14 Markdown Base64 payload.
pub fn encode_text_payload(content: &str) -> anyhow::Result<String> {
    encode_progress_payload(content, &zeroclaw_api::channel::ProgressPhase::Generic)
}

/// Encode a progress update payload: markdown `text` for fallback display,
/// plus structured fields from `phase` so rich clients can render in place
/// (e.g. update a tool bubble by `tool_call_id`).
pub fn encode_progress_payload(content: &str, phase: &zeroclaw_api::channel::ProgressPhase) -> anyhow::Result<String> {
    let mut inner = serde_json::json!({ "type": "markdown", "text": content });
    if let Some(obj) = inner.as_object_mut() {
        match phase {
            zeroclaw_api::channel::ProgressPhase::AgentStart { provider, model } => {
                obj.insert("phase".into(), serde_json::json!("agent_start"));
                obj.insert("provider".into(), serde_json::json!(provider));
                obj.insert("model".into(), serde_json::json!(model));
            }
            zeroclaw_api::channel::ProgressPhase::LlmRequest { messages_count } => {
                obj.insert("phase".into(), serde_json::json!("llm_request"));
                obj.insert("messages_count".into(), serde_json::json!(messages_count));
            }
            zeroclaw_api::channel::ProgressPhase::ToolStart { tool, tool_call_id } => {
                obj.insert("phase".into(), serde_json::json!("tool_start"));
                obj.insert("tool_name".into(), serde_json::json!(tool));
                if let Some(id) = tool_call_id {
                    obj.insert("tool_call_id".into(), serde_json::json!(id));
                }
            }
            zeroclaw_api::channel::ProgressPhase::ToolDone { tool, tool_call_id, success, elapsed_ms } => {
                obj.insert("phase".into(), serde_json::json!("tool_done"));
                obj.insert("tool_name".into(), serde_json::json!(tool));
                if let Some(id) = tool_call_id {
                    obj.insert("tool_call_id".into(), serde_json::json!(id));
                }
                obj.insert("success".into(), serde_json::json!(success));
                obj.insert("elapsed_ms".into(), serde_json::json!(elapsed_ms));
            }
            zeroclaw_api::channel::ProgressPhase::AgentEnd => {
                obj.insert("phase".into(), serde_json::json!("agent_end"));
            }
            zeroclaw_api::channel::ProgressPhase::Error { component } => {
                obj.insert("phase".into(), serde_json::json!("error"));
                obj.insert("component".into(), serde_json::json!(component));
            }
            zeroclaw_api::channel::ProgressPhase::Generic => {
                obj.insert("phase".into(), serde_json::json!("generic"));
            }
        }
    }
    let payload = serde_json::json!({ "type": 14, "content": inner });
    let json = serde_json::to_string(&payload)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(json))
}

pub fn detect_image_mime(content_type: Option<&str>, bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some("image/png".to_string());
    }
    if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg".to_string());
    }
    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    if bytes.len() >= 2 && bytes.starts_with(b"BM") {
        return Some("image/bmp".to_string());
    }
    content_type
        .and_then(|ct| ct.split(';').next())
        .map(|ct| ct.trim().to_lowercase())
        .filter(|ct| ct.starts_with("image/"))
}

pub async fn download_image_as_base64(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("dawnIM media: request failed: url={url}, err={e}")
            );
            return None;
        }
    };
    if !resp.status().is_success() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!("dawnIM media: HTTP {}: {url}", resp.status())
        );
        return None;
    }
    if let Some(cl) = resp.content_length()
        && cl > IMAGE_MAX_BYTES as u64
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!("dawnIM media: image too large ({cl} bytes): {url}")
        );
        return None;
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("dawnIM media: body read failed: {url}, {e}")
            );
            return None;
        }
    };
    if bytes.is_empty() || bytes.len() > IMAGE_MAX_BYTES {
        return None;
    }

    let mime = match detect_image_mime(content_type.as_deref(), &bytes) {
        Some(m) if SUPPORTED_IMAGE_MIMES.contains(&m.as_str()) => m,
        other => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("dawnIM media: unsupported MIME {other:?}: {url}")
            );
            return None;
        }
    };

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
}

pub async fn download_file_to_workspace(
    url: &str,
    downloads_dir: &std::path::Path,
    filename_hint: Option<&str>,
) -> Result<String, String> {
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        &format!(
            "download_file_to_workspace: url={url}, downloads_dir={}, filename_hint={filename_hint:?}",
            downloads_dir.display()
        )
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(format!("网络错误: {e}"));
        }
    };

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    if let Some(cl) = resp.content_length()
        && cl > FILE_MAX_BYTES as u64
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!("download_file_to_workspace: file too large {cl} > {FILE_MAX_BYTES} bytes")
        );
        return Err("文件超过 100MB 限制".to_string());
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Err(format!("读取响应失败: {e}"));
        }
    };

    if bytes.is_empty() || bytes.len() > FILE_MAX_BYTES {
        return Err("文件为空或超过大小限制".to_string());
    }

    let filename = if let Some(hint) = filename_hint {
        let is_high_entropy = hint.len() >= 32
            && hint.chars().filter(|&c| c == '-').count() <= 4
            && hint.chars().all(|c| c == '-' || c.is_alphanumeric());
        if is_high_entropy {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "download_file_to_workspace: hint {hint:?} looks like high-entropy, using URL filename"
                )
            );
            url.rsplit('/')
                .next()
                .unwrap_or("download")
                .split('?')
                .next()
                .unwrap_or("download")
                .to_string()
        } else {
            hint.to_string()
        }
    } else {
        url.rsplit('/')
            .next()
            .unwrap_or("download")
            .split('?')
            .next()
            .unwrap_or("download")
            .to_string()
    };

    if is_blocked_extension(&filename) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            &format!("download_file_to_workspace: blocked extension for filename={filename}")
        );
        return Err("不允许的文件类型".to_string());
    }

    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        &format!("download_file_to_workspace: creating dir {downloads_dir:?}")
    );
    if let Err(e) = tokio::fs::create_dir_all(&downloads_dir).await {
        return Err(format!("无法创建下载目录: {e}"));
    }

    let mut target_path = downloads_dir.join(&filename);
    let mut counter = 1;
    while target_path.exists() {
        let filename_str = filename.as_str();
        let stem = filename_str.rsplit('.').next().unwrap_or(filename_str);
        let ext = if filename_str.contains('.') {
            format!(".{}", filename_str.rsplit('.').next().unwrap_or(""))
        } else {
            String::new()
        };
        let new_filename = format!("{stem} ({counter}){ext}");
        target_path = downloads_dir.join(&new_filename);
        counter += 1;
    }

    if let Err(e) = tokio::fs::write(&target_path, &bytes).await {
        return Err(format!("写入文件失败: {e}"));
    }

    let result_path = target_path.to_str().unwrap().to_string();
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
            .with_outcome(::zeroclaw_log::EventOutcome::Success),
        &format!(
            "download_file_to_workspace: success, saved to {target_path:?}, result_path={result_path}"
        )
    );
    Ok(result_path)
}

pub fn extract_markdown_links(text: &str) -> Vec<(String, String, bool)> {
    let mut links = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        if let Some(cb) = after.find(']') {
            let alt = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                let url = inner[..pe]
                    .split_whitespace()
                    .next()
                    .unwrap_or(&inner[..pe]);
                links.push((alt, url.to_string(), true));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        break;
    }

    rest = text;
    while let Some(start) = rest.find('[') {
        if start > 0 && &rest[start - 1..start] == "!" {
            rest = &rest[start + 1..];
            continue;
        }

        let after = &rest[start + 1..];
        if let Some(cb) = after.find(']') {
            let text_content = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                let url = inner[..pe]
                    .split_whitespace()
                    .next()
                    .unwrap_or(&inner[..pe]);
                links.push((text_content, url.to_string(), false));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        rest = &rest[start + 1..];
    }

    links
}

pub fn extract_markdown_images(text: &str) -> Vec<(String, String)> {
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        if let Some(cb) = after.find(']') {
            let alt = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(')
                && let Some(pe) = inner.find(')')
            {
                images.push((alt, inner[..pe].to_string()));
                rest = &tail[pe + 1..];
                continue;
            }
        }
        break;
    }
    images
}

pub fn is_blocked_extension(filename: &str) -> bool {
    if let Some(ext) = filename.rsplit('.').next() {
        BLOCKED_EXTENSIONS.contains(&ext.to_lowercase().as_str())
    } else {
        false
    }
}

pub async fn process_markdown_resources(text: &str, downloads_dir: &std::path::Path) -> String {
    let links = extract_markdown_links(text);
    let mut result = text.to_string();

    for (alt, url, is_image) in links {
        if is_image {
            if let Some(marker) = download_image_as_base64(&url).await {
                result =
                    result.replace(&format!("![{alt}]({url})"), &format!("![{alt}]({marker})"));
            } else {
                result = result.replace(
                    &format!("![{alt}]({url})"),
                    &format!("![图片下载失败]({url})"),
                );
            }
        } else {
            match download_file_to_workspace(&url, downloads_dir, Some(&alt)).await {
                Ok(local_path) => {
                    result = result.replace(
                        &format!("[{alt}]({url})"),
                        &format!("[{alt}]({local_path})"),
                    );
                }
                Err(err_msg) => {
                    result = result.replace(
                        &format!("[{alt}]({url})"),
                        &format!("[{alt}]({url}) [下载失败: {err_msg}]"),
                    );
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn encode_text_payload_is_valid_base64_json() {
        let b64 = encode_text_payload("hello").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(val["type"], 14);
        assert_eq!(val["content"]["type"], "markdown");
        assert_eq!(val["content"]["text"], "hello");
    }

    #[test]
    fn encode_progress_payload_includes_tool_call_id_and_fields() {
        use base64::Engine;
        use zeroclaw_api::channel::ProgressPhase;
        let b64 = encode_progress_payload(
            "💭 shell completed (5ms)",
            &ProgressPhase::ToolDone {
                tool: "shell".into(),
                tool_call_id: Some("call_42".into()),
                success: true,
                elapsed_ms: 5,
            },
        )
        .expect("encode should succeed");
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(v["type"], 14);
        assert_eq!(v["content"]["tool_name"], "shell");
        assert_eq!(v["content"]["tool_call_id"], "call_42");
        assert_eq!(v["content"]["success"], true);
        assert_eq!(v["content"]["elapsed_ms"], 5);
        assert_eq!(v["content"]["phase"], "tool_done");
    }

    #[test]
    fn extract_no_images_from_plain_text() {
        assert!(extract_markdown_images("Hello world").is_empty());
    }

    #[test]
    fn extract_single_image() {
        let imgs = extract_markdown_images("![logo](https://example.com/logo.png)");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "logo");
        assert_eq!(imgs[0].1, "https://example.com/logo.png");
    }

    #[test]
    fn extract_multiple_images() {
        let text = "![a](https://a.com/a.png) text ![b](https://b.com/b.jpg)";
        let imgs = extract_markdown_images(text);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].1, "https://a.com/a.png");
        assert_eq!(imgs[1].1, "https://b.com/b.jpg");
    }

    #[test]
    fn detect_png_by_magic_bytes() {
        let png: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0];
        assert_eq!(detect_image_mime(None, png).as_deref(), Some("image/png"));
    }

    #[test]
    fn detect_jpeg_by_magic_bytes() {
        let jpeg: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0];
        assert_eq!(detect_image_mime(None, jpeg).as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn detect_mime_falls_back_to_content_type() {
        assert_eq!(
            detect_image_mime(Some("image/webp; charset=utf-8"), &[0u8; 4]).as_deref(),
            Some("image/webp")
        );
    }

    #[test]
    fn detect_non_image_returns_none() {
        assert!(detect_image_mime(Some("application/json"), &[0u8; 4]).is_none());
    }

    #[test]
    fn test_is_blocked_extension() {
        assert!(is_blocked_extension("script.exe"));
        assert!(is_blocked_extension("malware.js"));
        assert!(!is_blocked_extension("document.pdf"));
        assert!(!is_blocked_extension("data.txt"));
        assert!(!is_blocked_extension("no_extension"));
    }

    #[test]
    fn test_extract_markdown_links_images_only() {
        let text = "Check ![logo](https://example.com/logo.png) and ![photo](https://example.com/photo.jpg)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "logo");
        assert_eq!(links[0].1, "https://example.com/logo.png");
        assert!(links[0].2);
        assert!(links[1].2);
    }

    #[test]
    fn test_extract_markdown_links_files_only() {
        let text = "Download [document](https://example.com/file.pdf) and [data](https://example.com/data.csv)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "document");
        assert_eq!(links[0].1, "https://example.com/file.pdf");
        assert!(!links[0].2);
        assert!(!links[1].2);
    }

    #[test]
    fn test_extract_markdown_links_mixed() {
        let text = "See ![img](img.png) and [file](doc.pdf)";
        let links = extract_markdown_links(text);
        assert_eq!(links.len(), 2);
        assert!(links[0].2);
        assert!(!links[1].2);
    }
}
