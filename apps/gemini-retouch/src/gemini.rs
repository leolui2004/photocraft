//! Gemini Nano Banana 2.1 request and response handling.
//!
//! The interactions API takes the selection as an inline PNG plus a text prompt, and is asked
//! for a JPEG at one of the model's aspect ratios. The size tier is the smallest of 1K / 2K / 4K
//! whose frame is at least as large as the region that will be placed. Gemini only accepts
//! `image/jpeg` as `response_format.mime_type`. Each call logs the token usage from the response
//! (`usage` on the interactions API, `usageMetadata` on generateContent).

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::lang::{Lang, UsageLabel};

/// Model id. Nano Banana 2.1, the image-editing Gemini model.
pub const MODEL: &str = "gemini-nano-banana-2.1";

/// Default interactions endpoint. Overridable so tests can point at a local server.
pub const DEFAULT_URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

const TIER_NAMES: [&str; 3] = ["1K", "2K", "4K"];

/// One aspect ratio Nano Banana 2.1 accepts, and the pixels each tier returns.
///
/// The numbers are the Nano Banana 2.1 table in the Gemini image-generation docs. PhotoCraft's
/// menu gate in `crates/engine/src/gemini_cmds.rs` uses the same ratios and the same 4K sizes.
pub struct Ratio {
    pub label: &'static str,
    pub w: u32,
    pub h: u32,
    /// Width and height at 1K, 2K and 4K.
    tiers: [(u32, u32); 3],
}

/// Ratios the model will generate. Order is the order shown on the prompt page.
pub const RATIOS: &[Ratio] = &[
    Ratio { label: "1:1", w: 1, h: 1, tiers: [(1024, 1024), (2048, 2048), (4096, 4096)] },
    Ratio { label: "1:4", w: 1, h: 4, tiers: [(512, 2048), (1024, 4096), (2048, 8192)] },
    Ratio { label: "1:8", w: 1, h: 8, tiers: [(384, 3072), (768, 6144), (1536, 12288)] },
    Ratio { label: "2:3", w: 2, h: 3, tiers: [(848, 1264), (1696, 2528), (3392, 5056)] },
    Ratio { label: "3:2", w: 3, h: 2, tiers: [(1264, 848), (2528, 1696), (5056, 3392)] },
    Ratio { label: "3:4", w: 3, h: 4, tiers: [(896, 1200), (1792, 2400), (3584, 4800)] },
    Ratio { label: "4:1", w: 4, h: 1, tiers: [(2048, 512), (4096, 1024), (8192, 2048)] },
    Ratio { label: "4:3", w: 4, h: 3, tiers: [(1200, 896), (2400, 1792), (4800, 3584)] },
    Ratio { label: "4:5", w: 4, h: 5, tiers: [(928, 1152), (1856, 2304), (3712, 4608)] },
    Ratio { label: "5:4", w: 5, h: 4, tiers: [(1152, 928), (2304, 1856), (4608, 3712)] },
    Ratio { label: "8:1", w: 8, h: 1, tiers: [(3072, 384), (6144, 768), (12288, 1536)] },
    Ratio { label: "9:16", w: 9, h: 16, tiers: [(768, 1376), (1536, 2752), (3072, 5504)] },
    Ratio { label: "16:9", w: 16, h: 9, tiers: [(1376, 768), (2752, 1536), (5504, 3072)] },
    Ratio { label: "21:9", w: 21, h: 9, tiers: [(1584, 672), (3168, 1344), (6336, 2688)] },
];

pub fn ratio_labels() -> Vec<&'static str> {
    RATIOS.iter().map(|r| r.label).collect()
}

pub fn ratio_list() -> String {
    ratio_labels().join(", ")
}

pub fn find_ratio(label: &str) -> Option<&'static Ratio> {
    RATIOS.iter().find(|r| r.label == label)
}

pub fn unknown_ratio() -> String {
    format!("aspect ratio must be one of {}", ratio_list())
}

/// The Gemini ratio within one pixel of `width`×`height`. The closest wins; a tie keeps the
/// earlier entry, so a near-square stays `1:1` when nothing else is as close.
pub fn match_ratio(width: u32, height: u32) -> Result<&'static Ratio, String> {
    if width == 0 || height == 0 {
        return Err("the selection is empty".into());
    }
    let mut best: Option<(&Ratio, u64, u64)> = None;
    for ratio in RATIOS {
        let cross = cross(width, height, ratio);
        let den = u64::from(ratio.w.max(ratio.h));
        if den == 0 || cross > den {
            continue;
        }
        let replace = match best {
            None => true,
            Some((_, best_cross, best_den)) => cross.saturating_mul(best_den) < best_cross.saturating_mul(den),
        };
        if replace {
            best = Some((ratio, cross, den));
        }
    }
    best.map(|(ratio, _, _)| ratio).ok_or_else(|| format!("the selection must match a Gemini aspect ratio ({}); it is {width}×{height}", ratio_list()))
}

/// Smallest tier whose frame is at least `width`×`height`. Larger than 4K is an error.
pub fn image_size_for(width: u32, height: u32, ratio: &Ratio) -> Result<&'static str, String> {
    for (i, (tw, th)) in ratio.tiers.iter().enumerate() {
        if *tw >= width && *th >= height {
            return Ok(TIER_NAMES.get(i).copied().unwrap_or("4K"));
        }
    }
    let Some(&(max_w, max_h)) = ratio.tiers.last() else {
        return Err(format!("the selection is {width}×{height}; Gemini has no {label} frame", label = ratio.label));
    };
    Err(format!("the selection is {width}×{height}; the largest {} image Gemini can return is {max_w}×{max_h} (4K)", ratio.label))
}

pub struct Prepared {
    pub ratio: &'static str,
    pub image_size: &'static str,
}

/// Ratio and output tier for a selection that Gemini can answer.
pub fn prepare(width: u32, height: u32) -> Result<Prepared, String> {
    let ratio = match_ratio(width, height)?;
    let image_size = image_size_for(width, height, ratio)?;
    Ok(Prepared { ratio: ratio.label, image_size })
}

/// Drop at most one pixel so the rectangle sits closer to `ratio`. The top-left corner stays.
#[cfg(test)]
fn trim_to_ratio(width: u32, height: u32, ratio: &Ratio) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let mut best = (width, height);
    let mut best_err = cross(width, height, ratio);
    let candidates = [(width, height), (width.saturating_sub(1).max(1), height), (width, height.saturating_sub(1).max(1))];
    for (w, h) in candidates {
        let err = cross(w, h, ratio);
        if err < best_err {
            best = (w, h);
            best_err = err;
        }
    }
    best
}

fn cross(width: u32, height: u32, ratio: &Ratio) -> u64 {
    (width as u64).saturating_mul(u64::from(ratio.h)).abs_diff((height as u64).saturating_mul(u64::from(ratio.w)))
}

/// Largest `ratio` rectangle centered inside the selection. The full selection when it already
/// has that ratio (integer rounding can leave a one-pixel remainder).
pub fn inscribe(x: i32, y: i32, sel_w: u32, sel_h: u32, ratio: &Ratio) -> (i32, i32, u32, u32) {
    if ratio.w == 0 || ratio.h == 0 || sel_w == 0 || sel_h == 0 {
        return (x, y, sel_w.max(1), sel_h.max(1));
    }
    let sw = u64::from(sel_w);
    let sh = u64::from(sel_h);
    let aw = u64::from(ratio.w);
    let ah = u64::from(ratio.h);
    if sw.saturating_mul(ah) >= sh.saturating_mul(aw) {
        let height = sel_h;
        let width = u32::try_from(sh.saturating_mul(aw) / ah).unwrap_or(sel_w).min(sel_w).max(1);
        let dx = i32::try_from(sel_w.saturating_sub(width) / 2).unwrap_or(0);
        (x.saturating_add(dx), y, width, height)
    } else {
        let width = sel_w;
        let height = u32::try_from(sw.saturating_mul(ah) / aw).unwrap_or(sel_h).min(sel_h).max(1);
        let dy = i32::try_from(sel_h.saturating_sub(height) / 2).unwrap_or(0);
        (x, y.saturating_add(dy), width, height)
    }
}

/// JSON body for one edit.
pub fn interaction_body(prompt: &str, png: &[u8], aspect_ratio: &str, image_size: &str) -> Value {
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
            "aspect_ratio": aspect_ratio,
            "image_size": image_size
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

/// Something that turns a PNG and a prompt into a new image. The real client calls Gemini;
/// tests substitute a fake. `aspect_ratio` is one of [`RATIOS`]; `image_size` is `1K`, `2K` or `4K`.
pub trait Gemini: Send + Sync {
    fn edit(&self, png: &[u8], aspect_ratio: &str, image_size: &str, prompt: &str) -> Result<Vec<u8>, String>;
}

/// Blocking HTTPS client for the interactions API.
pub struct UreqGemini {
    keys: std::sync::Arc<crate::key::KeyStore>,
    url: String,
    lang: Lang,
}

impl UreqGemini {
    pub fn new(keys: std::sync::Arc<crate::key::KeyStore>, url: impl Into<String>, lang: Lang) -> Self {
        Self { keys, url: url.into(), lang }
    }
}

impl Gemini for UreqGemini {
    fn edit(&self, png: &[u8], aspect_ratio: &str, image_size: &str, prompt: &str) -> Result<Vec<u8>, String> {
        let api_key = self.keys.get();
        if api_key.is_empty() {
            return Err(self.lang.err_missing_key().to_string());
        }
        let body = interaction_body(prompt, png, aspect_ratio, image_size);
        let response = post_json(&self.url, &api_key, &body, image_size, self.lang)?;
        eprintln!("{}", usage_log_line(&response, image_size, self.lang));
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
    fn tier_follows_the_ratio_and_rejects_anything_past_4k() {
        let square = find_ratio("1:1").unwrap();
        assert_eq!(image_size_for(1, 1, square).unwrap(), "1K");
        assert_eq!(image_size_for(1024, 1024, square).unwrap(), "1K");
        assert_eq!(image_size_for(1025, 1024, square).unwrap(), "2K");
        assert_eq!(image_size_for(2048, 2048, square).unwrap(), "2K");
        assert_eq!(image_size_for(2049, 2048, square).unwrap(), "4K");
        assert_eq!(image_size_for(4096, 4096, square).unwrap(), "4K");
        assert!(image_size_for(4097, 4097, square).is_err());

        let wide = find_ratio("16:9").unwrap();
        assert_eq!(prepare(160, 90).unwrap().ratio, "16:9");
        assert_eq!(image_size_for(160, 90, wide).unwrap(), "1K");
        assert_eq!(image_size_for(1600, 900, wide).unwrap(), "2K");
        assert_eq!(image_size_for(5504, 3072, wide).unwrap(), "4K");
        assert!(prepare(5600, 3150).is_err(), "wider than the 4K 16:9 frame");
        assert!(prepare(100, 70).is_err(), "not one of the model's ratios");
        assert_eq!(prepare(5, 4).unwrap().ratio, "5:4");
        assert_eq!(prepare(1600, 901).unwrap().ratio, "16:9");
        assert_eq!(prepare(15, 16).unwrap().ratio, "1:1");
        assert_eq!(trim_to_ratio(15, 16, square), (15, 15));
        assert_eq!(trim_to_ratio(1600, 901, wide), (1600, 900));
        assert_eq!(trim_to_ratio(5, 4, find_ratio("5:4").unwrap()), (5, 4));
        let (x, y, w, h) = inscribe(10, 4, 16, 9, square);
        assert_eq!((x, y, w, h), (13, 4, 9, 9));
        assert_eq!(inscribe(0, 0, 1600, 900, wide), (0, 0, 1600, 900));
        assert!(ratio_list().contains("21:9") && ratio_list().contains("8:1") && ratio_list().contains("1:8"));
    }

    #[test]
    fn request_uses_the_chosen_ratio_and_a_tier_at_least_as_large_as_the_input() {
        let body = interaction_body("remove stray hair", b"\x89PNG", "16:9", "2K");
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["response_format"]["aspect_ratio"], "16:9");
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
            assert!(req.contains("\"aspect_ratio\":\"16:9\""), "{req}");
            assert!(req.contains("\"image_size\":\"2K\""), "{req}");
            assert!(req.contains("\"mime_type\":\"image/jpeg\""), "{req}");
            assert!(req.to_ascii_lowercase().contains("x-goog-api-key: test-key"));
            let data = base64::engine::general_purpose::STANDARD.encode(&expected);
            let body = json!({"output_image": {"mime_type": "image/png", "data": data}}).to_string();
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
        });
        let keys = std::sync::Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-unused-key"), Some("test-key".into())));
        let client = UreqGemini::new(keys, format!("http://{addr}/v1beta/interactions"), Lang::En);
        let out = client.edit(png, "16:9", "2K", "retouch skin").unwrap();
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
