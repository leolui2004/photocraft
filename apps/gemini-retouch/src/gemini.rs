//! Gemini Nano Banana 2.1 request and response handling.
//!
//! The interactions API takes the selection as an inline PNG plus a text prompt, and is asked
//! for a square JPEG at the smallest 1K / 2K / 4K tier that is not smaller than the input.
//! Gemini only accepts `image/jpeg` as `response_format.mime_type`. Each call logs the token
//! usage from the response (`usage` on the interactions API, `usageMetadata` on generateContent).

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::lang::{Lang, UsageLabel};

/// Model id. Nano Banana 2.1, the image-editing Gemini model.
pub const MODEL: &str = "gemini-nano-banana-2.1";

/// Default interactions endpoint. Overridable so tests can point at a local server.
pub const DEFAULT_URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

/// Largest square side the model can return (the 4K tier).
pub const MAX_SIDE: u32 = 4096;

/// 1K, 2K and 4K are 1024, 2048 and 4096 px on a 1:1 frame.
pub fn image_size_for(side: u32) -> &'static str {
    if side <= 1024 {
        "1K"
    } else if side <= 2048 {
        "2K"
    } else {
        "4K"
    }
}

/// JSON body for one edit. `side` is the selection's width (it is square).
pub fn interaction_body(prompt: &str, png: &[u8], side: u32) -> Value {
    let text =
        format!("{prompt}\n\nEdit the attached image in place. Change only what this instruction asks for, and keep the same framing, subject and background.");
    json!({
        "model": MODEL,
        "input": [
            {"type": "text", "text": text},
            {"type": "image", "mime_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)}
        ],
        "response_format": {
            "type": "image",
            "mime_type": "image/jpeg",
            "aspect_ratio": "1:1",
            "image_size": image_size_for(side)
        }
    })
}

/// Pull the last generated image out of an interactions response.
pub fn image_from_response(v: &Value) -> Result<Vec<u8>, String> {
    if let Some(msg) = error_message(v) {
        return Err(format!("Gemini: {msg}"));
    }
    if let Some(bytes) = image_block(v.get("output_image")).or_else(|| image_block(v.get("outputImage"))) {
        return bytes;
    }
    if let Some(steps) = v.get("steps").and_then(Value::as_array) {
        for step in steps.iter().rev() {
            let Some(content) = step.get("content").and_then(Value::as_array) else { continue };
            for block in content.iter().rev() {
                if block.get("type").and_then(Value::as_str) == Some("image")
                    && let Some(bytes) = image_block(Some(block))
                {
                    return bytes;
                }
            }
        }
    }
    let keys = v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    Err(format!("Gemini returned no image (response fields: {})", keys.join(", ")))
}

fn error_message(v: &Value) -> Option<String> {
    let err = v.get("error")?;
    if let Some(s) = err.as_str() {
        return Some(s.to_string());
    }
    err.get("message").and_then(Value::as_str).map(str::to_string)
}

fn image_block(v: Option<&Value>) -> Option<Result<Vec<u8>, String>> {
    let data = v?.get("data")?.as_str()?;
    Some(decode_b64(data))
}

fn decode_b64(data: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(cleaned.as_bytes()))
        .map_err(|_| "Gemini image data was not valid base64".into())
}

/// Width and height from a PNG's IHDR, or an error when the bytes are not a PNG.
pub fn png_size(png: &[u8]) -> Result<(u32, u32), String> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || png.get(..8) != Some(SIG) {
        return Err("the selection is not a PNG".into());
    }
    if png.get(12..16) != Some(b"IHDR") {
        return Err("the PNG is missing its IHDR chunk".into());
    }
    let w = be_u32(png.get(16..20).ok_or("truncated PNG")?)?;
    let h = be_u32(png.get(20..24).ok_or("truncated PNG")?)?;
    if w == 0 || h == 0 {
        return Err("the PNG has no pixels".into());
    }
    Ok((w, h))
}

fn be_u32(bytes: &[u8]) -> Result<u32, String> {
    let arr: [u8; 4] = bytes.try_into().map_err(|_| "truncated PNG".to_string())?;
    Ok(u32::from_be_bytes(arr))
}

/// Refuse a selection the model cannot answer with an image at least as large.
pub fn check_square(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the selection is empty".into());
    }
    if width != height {
        return Err(format!("the selection must be square (it is {width}×{height})"));
    }
    if width > MAX_SIDE {
        return Err(format!("the square is {width} px; the largest 1:1 image Gemini can return is {MAX_SIDE} px (4K)"));
    }
    Ok(())
}

/// Something that turns a PNG and a prompt into a new PNG. The real client calls Gemini;
/// tests substitute a fake.
pub trait Gemini: Send + Sync {
    fn edit(&self, png: &[u8], side: u32, prompt: &str) -> Result<Vec<u8>, String>;
}

/// Blocking HTTPS client for the interactions API.
pub struct UreqGemini {
    api_key: String,
    url: String,
    lang: Lang,
}

impl UreqGemini {
    pub fn new(api_key: impl Into<String>, url: impl Into<String>, lang: Lang) -> Self {
        Self { api_key: api_key.into(), url: url.into(), lang }
    }
}

impl Gemini for UreqGemini {
    fn edit(&self, png: &[u8], side: u32, prompt: &str) -> Result<Vec<u8>, String> {
        let body = interaction_body(prompt, png, side);
        let size = image_size_for(side);
        let response = post_json(&self.url, &self.api_key, &body, size, self.lang)?;
        eprintln!("{}", usage_log_line(&response, size, self.lang));
        image_from_response(&response)
    }
}

/// One server-log line. Recognized counters are spelled out; any other field is appended so a
/// new usage counter is not dropped. Image bytes elsewhere in the response are not included.
fn usage_log_line(v: &Value, image_size: &str, lang: Lang) -> String {
    let Some(raw_usage) = v.get("usage").or_else(|| v.get("usageMetadata")).or_else(|| v.get("usage_metadata")) else {
        let keys = v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        return lang.usage_missing(image_size, &keys.join(", "));
    };
    let Some(usage) = raw_usage.as_object() else {
        return lang.usage_body(image_size, &clip(&raw_usage.to_string()));
    };
    let summary = summarize_usage(usage, lang);
    let extra = extra_usage(usage);
    match (summary, extra) {
        (Some(summary), None) => lang.usage_body(image_size, &summary),
        (Some(summary), Some(extra)) => lang.usage_body(image_size, &format!("{summary} | {}", clip(&extra))),
        (None, _) => lang.usage_body(image_size, &clip(&Value::Object(usage.clone()).to_string())),
    }
}

fn summarize_usage(obj: &Map<String, Value>, lang: Lang) -> Option<String> {
    let input = first_u64(obj, &["total_input_tokens", "input_tokens", "prompt_token_count", "promptTokenCount"]);
    let output = first_u64(obj, &["total_output_tokens", "output_tokens", "candidates_token_count", "candidatesTokenCount"]);
    let thinking = first_u64(obj, &["total_thought_tokens", "thoughts_token_count", "thoughtsTokenCount", "thought_tokens"]);
    let cached = first_u64(obj, &["total_cached_tokens", "cached_content_token_count", "cachedContentTokenCount"]);
    let tool = first_u64(obj, &["total_tool_use_tokens"]);
    let total = first_u64(obj, &["total_tokens", "total_token_count", "totalTokenCount"]);
    let input_by = modalities(obj, &["input_tokens_by_modality", "input_tokens_details", "prompt_tokens_details", "promptTokensDetails"], lang);
    let output_by = modalities(obj, &["output_tokens_by_modality", "output_tokens_details", "candidates_tokens_details", "candidatesTokensDetails"], lang);
    let tool_by = modalities(obj, &["tool_use_tokens_by_modality"], lang);

    let mut parts = Vec::new();
    push_part(&mut parts, lang.usage_label(UsageLabel::Input), input, &input_by);
    push_part(&mut parts, lang.usage_label(UsageLabel::Output), output, &output_by);
    push_part(&mut parts, lang.usage_label(UsageLabel::Thinking), thinking, &[]);
    push_part(&mut parts, lang.usage_label(UsageLabel::Cached), cached, &[]);
    push_part(&mut parts, lang.usage_label(UsageLabel::Tool), tool, &tool_by);
    push_part(&mut parts, lang.usage_label(UsageLabel::Total), total, &[]);
    if parts.is_empty() { None } else { Some(parts.join(", ")) }
}

fn push_part(parts: &mut Vec<String>, label: &str, count: Option<u64>, detail: &[(String, u64)]) {
    let detail = format_modalities(detail);
    match (count, detail.is_empty()) {
        (None, true) => {}
        (None, false) => parts.push(format!("{label} ({detail})")),
        (Some(n), true) => parts.push(format!("{label} {n}")),
        (Some(n), false) => parts.push(format!("{label} {n} ({detail})")),
    }
}

fn format_modalities(items: &[(String, u64)]) -> String {
    items.iter().map(|(modality, tokens)| format!("{modality} {tokens}")).collect::<Vec<_>>().join(", ")
}

fn first_u64(obj: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| obj.get(*name).and_then(json_u64))
}

fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
}

fn modalities(obj: &Map<String, Value>, names: &[&str], lang: Lang) -> Vec<(String, u64)> {
    let Some(items) = names.iter().find_map(|name| obj.get(*name).and_then(Value::as_array)) else {
        return Vec::new();
    };
    items.iter().filter_map(|v| modality_count(v, lang)).collect()
}

fn modality_count(v: &Value, lang: Lang) -> Option<(String, u64)> {
    let tokens = ["tokens", "token_count", "tokenCount"].iter().find_map(|name| v.get(*name).and_then(json_u64))?;
    let modality = v.get("modality").and_then(Value::as_str).unwrap_or(lang.usage_label(UsageLabel::Unknown)).to_string();
    Some((modality, tokens))
}

fn extra_usage(obj: &Map<String, Value>) -> Option<String> {
    let mut extra = Map::new();
    for (key, value) in obj {
        if !is_known_usage_key(key) {
            extra.insert(key.clone(), value.clone());
        }
    }
    if extra.is_empty() { None } else { Some(Value::Object(extra).to_string()) }
}

fn is_known_usage_key(key: &str) -> bool {
    matches!(
        key,
        "total_input_tokens"
            | "input_tokens"
            | "prompt_token_count"
            | "promptTokenCount"
            | "total_output_tokens"
            | "output_tokens"
            | "candidates_token_count"
            | "candidatesTokenCount"
            | "total_thought_tokens"
            | "thoughts_token_count"
            | "thoughtsTokenCount"
            | "thought_tokens"
            | "total_cached_tokens"
            | "cached_content_token_count"
            | "cachedContentTokenCount"
            | "total_tool_use_tokens"
            | "total_tokens"
            | "total_token_count"
            | "totalTokenCount"
            | "input_tokens_by_modality"
            | "output_tokens_by_modality"
            | "tool_use_tokens_by_modality"
            | "prompt_tokens_details"
            | "promptTokensDetails"
            | "candidates_tokens_details"
            | "candidatesTokensDetails"
            | "input_tokens_details"
            | "output_tokens_details"
    )
}

fn post_json(url: &str, api_key: &str, body: &Value, image_size: &str, lang: Lang) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(20)).timeout(std::time::Duration::from_secs(180)).build();
    let text = body.to_string();
    match agent.post(url).set("x-goog-api-key", api_key).set("Content-Type", "application/json").send_string(&text) {
        Ok(resp) => {
            let raw = resp.into_string().map_err(|e| format!("could not read the Gemini response: {e}"))?;
            serde_json::from_str(&raw).map_err(|_| "Gemini returned a response that was not JSON".to_string())
        }
        Err(ureq::Error::Status(code, resp)) => {
            let raw = resp.into_string().unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(&raw).ok();
            if let Some(v) = &parsed
                && v.get("usage").or_else(|| v.get("usageMetadata")).or_else(|| v.get("usage_metadata")).is_some()
            {
                eprintln!("{}", usage_log_line(v, image_size, lang));
            }
            let msg = parsed.as_ref().and_then(error_message).unwrap_or(raw);
            Err(format!("Gemini returned HTTP {code}: {}", clip(&msg)))
        }
        Err(e) => Err(format!("the Gemini request failed: {e}")),
    }
}

fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(400).collect();
    if s.chars().nth(400).is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;

    #[test]
    fn tier_is_never_smaller_than_the_input_until_the_api_cap() {
        assert_eq!(image_size_for(1), "1K");
        assert_eq!(image_size_for(1024), "1K");
        assert_eq!(image_size_for(1025), "2K");
        assert_eq!(image_size_for(2048), "2K");
        assert_eq!(image_size_for(2049), "4K");
        assert_eq!(image_size_for(4096), "4K");
        assert!(check_square(4096, 4096).is_ok());
        assert!(check_square(4097, 4097).is_err());
        assert!(check_square(40, 30).is_err());
    }

    #[test]
    fn request_is_square_and_at_least_as_large_as_the_input() {
        let body = interaction_body("remove stray hair", b"\x89PNG", 1500);
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["response_format"]["aspect_ratio"], "1:1");
        assert_eq!(body["response_format"]["image_size"], "2K");
        assert_eq!(body["response_format"]["mime_type"], "image/jpeg");
        let text = body["input"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("remove stray hair"));
        assert_eq!(body["input"][1]["mime_type"], "image/png");
        assert!(body["input"][1]["data"].as_str().unwrap_or("").starts_with("iVBORw"));
    }

    #[test]
    fn image_comes_back_from_either_response_shape() {
        let png = b"png-bytes";
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        let direct = json!({"output_image": {"data": data, "mime_type": "image/png"}});
        assert_eq!(image_from_response(&direct).unwrap(), png);
        let steps = json!({"steps": [
            {"type": "model_output", "content": [
                {"type": "text", "text": "done"},
                {"type": "image", "data": data}
            ]}
        ]});
        assert_eq!(image_from_response(&steps).unwrap(), png);
        assert!(image_from_response(&json!({"error": {"message": "blocked"}})).unwrap_err().contains("blocked"));
        assert!(image_from_response(&json!({"steps": []})).is_err());
    }

    #[test]
    fn png_header_size_and_a_local_round_trip() {
        // 1×1 RGB PNG (red). IHDR width and height are the two big-endian u32s after the tag.
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x5d, 0xec, 0xea, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        assert_eq!(png_size(png).unwrap(), (1, 1));
        assert!(png_size(b"nope").is_err());

        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let expected = png.to_vec();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = server.accept().unwrap();
            let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let req = loop {
                let n = std::io::Read::read(&mut sock, &mut tmp).unwrap();
                assert!(n > 0, "client closed before sending the request");
                buf.extend_from_slice(&tmp[..n]);
                let req = String::from_utf8_lossy(&buf).into_owned();
                let Some((head, body)) = req.split_once("\r\n\r\n") else { continue };
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.split_once(':').filter(|(n, _)| n.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break req;
                }
            };
            assert!(req.contains("\"aspect_ratio\":\"1:1\""), "{req}");
            assert!(req.contains("\"image_size\":\"2K\""), "{req}");
            assert!(req.contains("\"mime_type\":\"image/jpeg\""), "{req}");
            assert!(req.to_ascii_lowercase().contains("x-goog-api-key: test-key"));
            let data = base64::engine::general_purpose::STANDARD.encode(&expected);
            let body = json!({"output_image": {"mime_type": "image/png", "data": data}}).to_string();
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
        });
        let client = UreqGemini::new("test-key", format!("http://{addr}/v1beta/interactions"), Lang::En);
        let out = client.edit(png, 1500, "retouch skin").unwrap();
        assert_eq!(out, png);
        handle.join().unwrap();
    }

    #[test]
    fn usage_line_reads_interactions_and_generate_content_shapes() {
        let interactions = json!({"usage": {
            "total_input_tokens": 1840,
            "input_tokens_by_modality": [{"modality": "TEXT", "tokens": 40}, {"modality": "IMAGE", "tokens": 1800}],
            "total_output_tokens": 1680,
            "output_tokens_by_modality": [{"modality": "IMAGE", "tokens": 1680}],
            "total_thought_tokens": 12,
            "total_tokens": 3532
        }});
        assert_eq!(
            usage_log_line(&interactions, "2K", Lang::En),
            "Gemini usage (2K): input 1840 (TEXT 40, IMAGE 1800), output 1680 (IMAGE 1680), thinking 12, total 3532"
        );
        assert_eq!(
            usage_log_line(&interactions, "2K", Lang::Ja),
            "Gemini 使用量 (2K): 入力 1840 (TEXT 40, IMAGE 1800), 出力 1680 (IMAGE 1680), 思考 12, 合計 3532"
        );
        assert_eq!(
            usage_log_line(&interactions, "2K", Lang::ZhHant),
            "Gemini 用量 (2K): 輸入 1840 (TEXT 40, IMAGE 1800), 輸出 1680 (IMAGE 1680), 思考 12, 總計 3532"
        );

        let generate_content = json!({"usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 1120,
            "thoughtsTokenCount": 5,
            "totalTokenCount": 1225,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 100}],
            "candidatesTokensDetails": [{"modality": "IMAGE", "tokenCount": 1120}]
        }});
        assert_eq!(
            usage_log_line(&generate_content, "1K", Lang::En),
            "Gemini usage (1K): input 100 (TEXT 100), output 1120 (IMAGE 1120), thinking 5, total 1225"
        );

        let with_extra = json!({"usage_metadata": {"total_tokens": 10, "traffic_type": "ON_DEMAND"}});
        let line = usage_log_line(&with_extra, "4K", Lang::En);
        assert!(line.starts_with("Gemini usage (4K): total 10 | "), "{line}");
        assert!(line.contains("traffic_type"), "{line}");
        assert!(line.contains("ON_DEMAND"), "{line}");
        let line = usage_log_line(&with_extra, "4K", Lang::Ja);
        assert!(line.starts_with("Gemini 使用量 (4K): 合計 10 | "), "{line}");
        let line = usage_log_line(&with_extra, "4K", Lang::ZhHant);
        assert!(line.starts_with("Gemini 用量 (4K): 總計 10 | "), "{line}");
        assert!(line.contains("traffic_type"), "{line}");
        assert!(line.contains("ON_DEMAND"), "{line}");
    }

    #[test]
    fn usage_line_does_not_print_the_image_when_usage_is_missing() {
        let v = json!({"output_image": {"data": "not-the-usage"}, "status": "completed"});
        let line = usage_log_line(&v, "2K", Lang::En);
        assert!(line.contains("not reported"), "{line}");
        assert!(line.contains("output_image"), "{line}");
        assert!(!line.contains("not-the-usage"), "{line}");
        assert!(usage_log_line(&v, "2K", Lang::Ja).contains("報告なし"));
        assert!(usage_log_line(&v, "2K", Lang::ZhHant).contains("未回報"));

        let unnamed = json!({"usage": {"input_tokens_by_modality": [{"tokens": 3}]}});
        assert_eq!(usage_log_line(&unnamed, "1K", Lang::En), "Gemini usage (1K): input (unknown 3)");
        assert_eq!(usage_log_line(&unnamed, "1K", Lang::Ja), "Gemini 使用量 (1K): 入力 (不明 3)");
        assert_eq!(usage_log_line(&unnamed, "1K", Lang::ZhHant), "Gemini 用量 (1K): 輸入 (未知 3)");
    }
}
