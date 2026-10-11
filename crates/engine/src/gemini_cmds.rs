//! Generative retouch of a square selection.
//!
//! PhotoCraft crops the square and posts it to a separate local backend
//! (`apps/gemini-retouch`). The prompt is typed on that backend's page, which calls
//! Gemini Nano Banana 2.1. PhotoCraft then places the returned image on a new layer,
//! scaled to the selection. Passing `prompt` does the round trip in one command so
//! agents do not need the page.

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const SEND: &str = "edit.generativeRetouch";
const PLACE: &str = "edit.generativeRetouch.place";
const DEFAULT_URL: &str = "http://127.0.0.1:8787";
/// Largest square the backend will accept (Gemini's 4K 1:1 frame).
const MAX_SIDE: u32 = 4096;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: SEND.into(), msg: msg.into() }
}

fn square(doc: &Document) -> std::result::Result<Rect, String> {
    let Some(sel) = doc.selection.as_ref() else {
        return Err("select a square region first".into());
    };
    let mut b = sel.content_bounds();
    if b.is_empty() {
        return Err("the selection is empty".into());
    }
    // A 1:1 marquee is square in document space, but floor/ceil (or one pixel of anti-aliasing)
    // can leave the mask one pixel off. Use the inscribed square so Generative Retouch stays enabled.
    let (w, h) = (b.width(), b.height());
    if w != h {
        if w.abs_diff(h) != 1 {
            return Err(format!("the selection must be square (it is {w}×{h})"));
        }
        let side = w.min(h);
        b = Rect::from_xywh(b.x0, b.y0, side, side);
    }
    if !doc.bounds().contains_rect(&b) {
        return Err("the square selection must lie entirely on the canvas".into());
    }
    if b.width() > MAX_SIDE {
        return Err(format!("the square is {} px; the largest 1:1 image Gemini can return is {MAX_SIDE} px", b.width()));
    }
    Ok(b)
}

fn enabled_send(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    square(&d.doc).map(|_| ())
}

fn enabled_place(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

struct Opts {
    origin: Origin,
    prompt: Option<String>,
    open_page: bool,
}

struct Origin {
    host: String,
    port: u16,
}

impl Origin {
    fn page(&self) -> String {
        format!("http://{}:{}/", self.host, self.port)
    }
}

fn opts(p: &Value) -> Result<Opts> {
    if p.get("url").is_some_and(|v| !v.is_string()) {
        return Err(bad("`url` must be a string"));
    }
    if p.get("prompt").is_some_and(|v| !v.is_string()) {
        return Err(bad("`prompt` must be a string"));
    }
    if p.get("open").is_some_and(|v| !v.is_boolean()) {
        return Err(bad("`open` must be a boolean"));
    }
    let url = p.get("url").and_then(Value::as_str).unwrap_or(DEFAULT_URL);
    let origin = parse_origin(url).map_err(bad)?;
    let prompt = p.get("prompt").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if prompt.as_ref().is_some_and(|s| s.chars().count() > 4_000) {
        return Err(bad("the prompt is too long"));
    }
    let open_page = p.get("open").and_then(Value::as_bool).unwrap_or(true);
    Ok(Opts { origin, prompt, open_page })
}

fn parse_origin(url: &str) -> std::result::Result<Origin, String> {
    let rest = url.trim().strip_prefix("http://").ok_or_else(|| "url must be http://host:port (the retouch backend is local)".to_string())?;
    if rest.is_empty() || rest.contains("://") || rest.contains('@') {
        return Err("url must be http://host:port".into());
    }
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    if !path.is_empty() {
        return Err("url must be an origin such as http://127.0.0.1:8787".into());
    }
    if hostport.is_empty() || hostport.len() > 200 {
        return Err("bad url".into());
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().map_err(|_| "bad port".to_string())?;
            (host, port)
        }
        None => (hostport, 80),
    };
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err("bad host".into());
    }
    Ok(Origin { host: host.to_string(), port })
}

fn send(s: &mut Session, p: &Value) -> Result<Value> {
    let opts = opts(p)?;
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let rect = square(doc).map_err(EngineError::Other)?;
    let png = selection_png(doc, rect)?;
    if let Some(prompt) = opts.prompt.clone() {
        let origin = opts.origin;
        return crate::jobs::edit_job(
            s,
            "Generative Retouch",
            move |doc, active, ctx| {
                ctx.progress(0.05, "Sending the selection");
                ctx.check()?;
                let got = http::post_edit(&origin, &png, rect, &prompt)?;
                ctx.check()?;
                ctx.progress(0.85, "Placing the layer");
                let id = place_layer(doc, active, &got.png, got.x, got.y, got.width, got.height)?;
                Ok((id.0, got.x, got.y, got.width, got.height, got.image_size))
            },
            |(id, x, y, width, height, image_size)| json!({"layer": id, "x": x, "y": y, "width": width, "height": height, "imageSize": image_size}),
        );
    }
    http::post_selection(&opts.origin, &png, rect)?;
    let page = opts.origin.page();
    let opened = if opts.open_page { open_page(&page) } else { false };
    Ok(json!({
        "awaitingPrompt": true,
        "page": page,
        "opened": opened,
        "x": rect.x0,
        "y": rect.y0,
        "width": rect.width(),
        "height": rect.height(),
    }))
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let opts = opts(p)?;
    let got = http::get_result(&opts.origin)?;
    place_fetched(s, &got)
}

fn place_fetched(s: &mut Session, got: &Fetched) -> Result<Value> {
    let id = s.edit("Generative Retouch", |doc, active| {
        let id = place_layer(doc, active, &got.png, got.x, got.y, got.width, got.height)?;
        Ok(id)
    })?;
    Ok(json!({
        "layer": id.0,
        "x": got.x,
        "y": got.y,
        "width": got.width,
        "height": got.height,
        "imageSize": got.image_size,
    }))
}

/// Composite of `rect`, flattened over white, as a PNG.
fn selection_png(doc: &Document, rect: Rect) -> Result<Vec<u8>> {
    let buf = photocraft_compose::render(doc, rect);
    let w = rect.width();
    let h = rect.height();
    let n = (w as usize).checked_mul(h as usize).ok_or_else(|| EngineError::Other("the selection is too large".into()))?;
    if buf.px.len() != n {
        return Err(EngineError::Other("could not read the selection".into()));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve(n.saturating_mul(4)).map_err(|_| EngineError::Other("the selection is too large".into()))?;
    for p in &buf.px {
        let a = finite01(p[3]);
        bytes.push(to_u8(finite01(p[0]) * a + (1.0 - a)));
        bytes.push(to_u8(finite01(p[1]) * a + (1.0 - a)));
        bytes.push(to_u8(finite01(p[2]) * a + (1.0 - a)));
        bytes.push(255);
    }
    let img = Image::from_u8(w, h, ChannelLayout::Rgba, bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).map_err(|e| EngineError::Other(e.to_string()))
}

fn finite01(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn to_u8(v: f32) -> u8 {
    (finite01(v) * 255.0).round() as u8
}

fn place_layer(doc: &mut Document, active: &mut Option<LayerId>, png: &[u8], x: i32, y: i32, w: u32, h: u32) -> Result<LayerId> {
    let img = photocraft_codecs::decode(png).map_err(|e| EngineError::Other(format!("the retouch image could not be read: {e}")))?;
    let sw = img.width();
    let sh = img.height();
    if sw == 0 || sh == 0 || sw > MAX_SIDE || sh > MAX_SIDE {
        return Err(EngineError::Other(format!("the retouch image is {sw}×{sh}, which this command will not place")));
    }
    let flat = img.to_rgba_f32();
    let src_n = (sw as usize).checked_mul(sh as usize).ok_or_else(|| EngineError::Other("the retouch image is too large".into()))?;
    if flat.len() != src_n.saturating_mul(4) {
        return Err(EngineError::Other("the retouch image could not be read".into()));
    }
    let mut rgba = Vec::new();
    rgba.try_reserve(src_n).map_err(|_| EngineError::Other("the retouch image is too large".into()))?;
    let (chunks, _) = flat.as_chunks::<4>();
    for px in chunks {
        rgba.push([finite01(px[0]), finite01(px[1]), finite01(px[2]), finite01(px[3])]);
    }
    let (dw, dh) = if w == 0 || h == 0 { (sw, sh) } else { (w, h) };
    let fitted = fit_rgba(&rgba, sw, sh, dw, dh)?;
    let fmt = doc.pixel_format();
    let target = PixelFormat::new(fmt.mode, fmt.sample, true);
    let surf = rgba_surface(target, &fitted, x, y, dw, dh)?;
    let mut layer = Layer::raster(doc.next_layer_name("Generative Retouch"), target);
    *crate::pixels_mut(&mut layer)? = surf;
    let id = doc.insert_above(*active, layer);
    *active = Some(id);
    Ok(id)
}

fn rgba_surface(fmt: PixelFormat, rgba: &[[f32; 4]], x: i32, y: i32, w: u32, h: u32) -> Result<Surface> {
    let n = fmt.channels();
    let pixels = (w as usize).checked_mul(h as usize).ok_or_else(|| EngineError::Other("the retouch image is too large".into()))?;
    if rgba.len() != pixels {
        return Err(EngineError::Other("the retouch image could not be placed".into()));
    }
    let mut data = Vec::new();
    let bytes = pixels.checked_mul(n).ok_or_else(|| EngineError::Other("the retouch image is too large".into()))?;
    data.try_reserve(bytes).map_err(|_| EngineError::Other("the retouch image is too large".into()))?;
    for px in rgba {
        let enc = from_rgba(&fmt, *px);
        if enc.len() != n {
            return Err(EngineError::Other("the retouch image could not be placed".into()));
        }
        data.extend_from_slice(&enc);
    }
    let mut surf = Surface::new(fmt);
    let rect = Rect::from_xywh(x, y, w, h);
    if data.len() != bytes {
        return Err(EngineError::Other("the retouch image could not be placed".into()));
    }
    surf.write_region(rect, &data);
    surf.prune();
    Ok(surf)
}

/// Scale `src` (sw×sh) to exactly dw×dh. Same size is a copy.
fn fit_rgba(src: &[[f32; 4]], sw: u32, sh: u32, dw: u32, dh: u32) -> Result<Vec<[f32; 4]>> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Err(EngineError::Other("the retouch image is empty".into()));
    }
    let expect = (sw as usize).checked_mul(sh as usize).ok_or_else(|| EngineError::Other("the retouch image is too large".into()))?;
    if src.len() != expect {
        return Err(EngineError::Other("the retouch image could not be placed".into()));
    }
    if sw == dw && sh == dh {
        return Ok(src.to_vec());
    }
    let n = (dw as usize).checked_mul(dh as usize).ok_or_else(|| EngineError::Other("the retouch image is too large".into()))?;
    let mut out = Vec::new();
    out.try_reserve(n).map_err(|_| EngineError::Other("the retouch image is too large".into()))?;
    for y in 0..dh {
        let fy = (y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
        for x in 0..dw {
            let fx = (x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            out.push(bilinear(src, sw, sh, fx, fy));
        }
    }
    Ok(out)
}

fn bilinear(src: &[[f32; 4]], sw: u32, sh: u32, x: f32, y: f32) -> [f32; 4] {
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let x0 = (x.floor().max(0.0) as u32).min(sw - 1);
    let y0 = (y.floor().max(0.0) as u32).min(sh - 1);
    let x1 = (x0 + 1).min(sw - 1);
    let y1 = (y0 + 1).min(sh - 1);
    let tx = (x - x0 as f32).clamp(0.0, 1.0);
    let ty = (y - y0 as f32).clamp(0.0, 1.0);
    let at = |xx: u32, yy: u32| -> [f32; 4] {
        src.get(yy as usize * sw as usize + xx as usize).copied().unwrap_or([0.0; 4])
    };
    let lerp = |a: [f32; 4], b: [f32; 4], t: f32| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
    lerp(lerp(at(x0, y0), at(x1, y0), tx), lerp(at(x0, y1), at(x1, y1), tx), ty).map(finite01)
}

struct Fetched {
    png: Vec<u8>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    image_size: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn open_page(url: &str) -> bool {
    #[cfg(windows)]
    let spawned = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(url).spawn();
    spawned.is_ok()
}

#[cfg(target_arch = "wasm32")]
fn open_page(_url: &str) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
mod http {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    use serde_json::json;

    use super::{Fetched, Origin};
    use crate::{EngineError, Result};
    use photocraft_geom::Rect;

    pub fn post_selection(origin: &Origin, png: &[u8], rect: Rect) -> Result<()> {
        let body = selection_json(png, rect, None);
        let resp = exchange(origin, "POST", "/selection", "application/json", body.as_bytes(), Duration::from_secs(15))?;
        expect_ok(&resp, "upload the selection")
    }

    pub fn post_edit(origin: &Origin, png: &[u8], rect: Rect, prompt: &str) -> Result<Fetched> {
        let body = selection_json(png, rect, Some(prompt));
        let resp = exchange(origin, "POST", "/v1/edit", "application/json", body.as_bytes(), Duration::from_secs(180))?;
        fetched(&resp, rect)
    }

    pub fn get_result(origin: &Origin) -> Result<Fetched> {
        let resp = exchange(origin, "GET", "/result", "application/json", &[], Duration::from_secs(15))?;
        fetched(&resp, Rect::EMPTY)
    }

    fn selection_json(png: &[u8], rect: Rect, prompt: Option<&str>) -> String {
        let mut v = json!({
            "png": b64(png),
            "x": rect.x0,
            "y": rect.y0,
            "width": rect.width(),
            "height": rect.height(),
        });
        if let Some(prompt) = prompt
            && let Some(obj) = v.as_object_mut()
        {
            obj.insert("prompt".into(), serde_json::Value::String(prompt.to_string()));
        }
        v.to_string()
    }

    fn fetched(resp: &Response, fallback: Rect) -> Result<Fetched> {
        if !(200..300).contains(&resp.status) {
            return Err(EngineError::Other(error_text(resp)));
        }
        if !resp.content_type.starts_with("image/") {
            return Err(EngineError::Other(error_text(resp)));
        }
        let x = header_i32(resp, "x-origin-x").unwrap_or(fallback.x0);
        let y = header_i32(resp, "x-origin-y").unwrap_or(fallback.y0);
        let width = header_u32(resp, "x-origin-width").unwrap_or(fallback.width());
        let height = header_u32(resp, "x-origin-height").unwrap_or(fallback.height());
        Ok(Fetched { png: resp.body.clone(), x, y, width, height, image_size: header(resp, "x-image-size") })
    }

    fn expect_ok(resp: &Response, what: &str) -> Result<()> {
        if (200..300).contains(&resp.status) {
            Ok(())
        } else {
            Err(EngineError::Other(format!("could not {what}: {}", error_text(resp))))
        }
    }

    fn error_text(resp: &Response) -> String {
        let v: serde_json::Value = serde_json::from_slice(&resp.body).unwrap_or(serde_json::Value::Null);
        if let Some(s) = v.get("error").and_then(serde_json::Value::as_str) {
            return s.to_string();
        }
        let text = String::from_utf8_lossy(&resp.body);
        let t: String = text.chars().take(200).collect();
        if t.is_empty() {
            format!("the retouch backend returned HTTP {}", resp.status)
        } else {
            format!("the retouch backend returned HTTP {}: {t}", resp.status)
        }
    }

    struct Response {
        status: u16,
        content_type: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn header(resp: &Response, name: &str) -> Option<String> {
        resp.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }

    fn header_i32(resp: &Response, name: &str) -> Option<i32> {
        header(resp, name)?.parse().ok()
    }

    fn header_u32(resp: &Response, name: &str) -> Option<u32> {
        header(resp, name)?.parse().ok()
    }

    fn exchange(origin: &Origin, method: &str, path: &str, content_type: &str, body: &[u8], read_for: Duration) -> Result<Response> {
        let addr = format!("{}:{}", origin.host, origin.port);
        let sock_addr = addr.to_socket_addrs().map_err(|e| unreachable_backend(origin, &e.to_string()))?.next().ok_or_else(|| unreachable_backend(origin, "no address"))?;
        let mut stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(2)).map_err(|e| unreachable_backend(origin, &e.to_string()))?;
        stream.set_read_timeout(Some(read_for)).map_err(|e| EngineError::Other(e.to_string()))?;
        stream.set_write_timeout(Some(Duration::from_secs(15))).map_err(|e| EngineError::Other(e.to_string()))?;
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            origin.host_header(),
            body.len()
        );
        stream.write_all(head.as_bytes()).map_err(|e| EngineError::Other(e.to_string()))?;
        stream.write_all(body).map_err(|e| EngineError::Other(e.to_string()))?;
        read_response(&mut stream).map_err(|e| EngineError::Other(format!("the retouch backend closed the connection: {e}")))
    }

    impl Origin {
        fn host_header(&self) -> String {
            if self.port == 80 { self.host.clone() } else { format!("{}:{}", self.host, self.port) }
        }
    }

    fn unreachable_backend(origin: &Origin, detail: &str) -> EngineError {
        EngineError::Other(format!(
            "PhotoCraft Retouch is not running at {}. Start it and leave its window open, then try again ({detail}).",
            origin.page()
        ))
    }

    fn read_response(stream: &mut TcpStream) -> std::io::Result<Response> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 8192];
        let header_end = loop {
            if buf.len() > 64 * 1024 {
                return Err(std::io::Error::other("response headers are too large"));
            }
            let n = read_some(stream, &mut tmp)?;
            buf.extend_from_slice(&tmp[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
        };
        let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| std::io::Error::other("response headers are not UTF-8"))?;
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status = status_line.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut content_type = String::new();
        let mut length: Option<usize> = None;
        let mut headers = Vec::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else { continue };
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("content-type") {
                content_type = value.clone();
            } else if name.eq_ignore_ascii_case("content-length")
                && let Ok(n) = value.parse::<usize>()
            {
                length = Some(n);
            }
            headers.push((name, value));
        }
        let length = length.unwrap_or(0);
        if length > 32 * 1024 * 1024 {
            return Err(std::io::Error::other("the retouch image is too large"));
        }
        let mut body = buf.split_off(header_end + 4);
        while body.len() < length {
            let n = read_some(stream, &mut tmp)?;
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(length);
        Ok(Response { status, content_type, headers, body })
    }

    fn read_some(stream: &mut TcpStream, tmp: &mut [u8]) -> std::io::Result<usize> {
        match stream.read(tmp) {
            Ok(0) => Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed")),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => read_some(stream, tmp),
            Err(e) => Err(e),
        }
    }

    fn b64(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i + 3 <= data.len() {
            let b0 = data.get(i).copied().unwrap_or(0) as u32;
            let b1 = data.get(i + 1).copied().unwrap_or(0) as u32;
            let b2 = data.get(i + 2).copied().unwrap_or(0) as u32;
            push_quartet(&mut out, (b0 << 16) | (b1 << 8) | b2, 4, T);
            i += 3;
        }
        let rest = data.len() - i;
        if rest == 1 {
            let b0 = data.get(i).copied().unwrap_or(0) as u32;
            push_quartet(&mut out, b0 << 16, 2, T);
            out.push('=');
            out.push('=');
        } else if rest == 2 {
            let b0 = data.get(i).copied().unwrap_or(0) as u32;
            let b1 = data.get(i + 1).copied().unwrap_or(0) as u32;
            push_quartet(&mut out, (b0 << 16) | (b1 << 8), 3, T);
            out.push('=');
        }
        out
    }

    fn push_quartet(out: &mut String, n: u32, chars: u32, table: &[u8]) {
        for k in 0..chars {
            let idx = ((n >> (18 - 6 * k)) & 63) as usize;
            if let Some(c) = table.get(idx) {
                out.push(*c as char);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod http {
    use super::Fetched;
    use super::Origin;
    use crate::{EngineError, Result};
    use photocraft_geom::Rect;

    fn unsupported() -> EngineError {
        EngineError::Other("generative retouch needs the desktop app and the local retouch backend".into())
    }

    pub fn post_selection(_origin: &Origin, _png: &[u8], _rect: Rect) -> Result<()> {
        Err(unsupported())
    }
    pub fn post_edit(_origin: &Origin, _png: &[u8], _rect: Rect, _prompt: &str) -> Result<Fetched> {
        Err(unsupported())
    }
    pub fn get_result(_origin: &Origin) -> Result<Fetched> {
        Err(unsupported())
    }
}

pub fn specs() -> Vec<CommandSpec> {
    // Place is listed first so the menu inserts Retouch directly under Content-Aware Fill
    // and Place just after it (see `PLACE_AFTER` in the UI).
    vec![
        CommandSpec {
            id: PLACE,
            label: "Place Generative Result",
            menu: &["Edit"],
            shortcut: None,
            params: r##"{"url":"http://127.0.0.1:8787"?} — fetch the image the retouch backend finished and add it as a new layer on the selection"##,
            enabled: enabled_place,
            run: place,
            journal: true,
        },
        CommandSpec {
            id: SEND,
            label: "Generative Retouch",
            menu: &["Edit"],
            shortcut: None,
            params: r##"{"prompt":str? (when set, the backend runs Gemini immediately and the image is placed; when omitted, the selection is uploaded and the prompt is entered at the backend page),"url":"http://127.0.0.1:8787"?,"open":bool=true (open the prompt page when no prompt was given)} — needs a square selection"##,
            enabled: enabled_send,
            run: send,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
    use serde_json::json;

    use super::*;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        let img = Image::from_u8(w, h, ChannelLayout::Rgba, data).unwrap();
        photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap()
    }

    struct Slot {
        stop: bool,
        place: Option<(i64, i64, u64, u64)>,
    }

    struct Mock {
        addr: String,
        slot: Arc<Mutex<Slot>>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.slot.lock().unwrap_or_else(PoisonError::into_inner).stop = true;
            let _ = TcpStream::connect(self.addr.trim_start_matches("http://"));
        }
    }

    fn mock(png: Vec<u8>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let slot = Arc::new(Mutex::new(Slot { stop: false, place: None }));
        let slot_w = Arc::clone(&slot);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if slot_w.lock().unwrap_or_else(PoisonError::into_inner).stop {
                    break;
                }
                let Ok(mut sock) = conn else { continue };
                let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
                if handle_mock(&mut sock, &png, &slot_w).is_err() {
                    break;
                }
            }
        });
        Mock { addr: format!("http://{addr}"), slot }
    }

    fn handle_mock(sock: &mut TcpStream, result_png: &[u8], slot: &Mutex<Slot>) -> std::io::Result<()> {
        let mut buf = vec![0u8; 256 * 1024];
        let n = sock.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let req = String::from_utf8_lossy(&buf[..n]);
        let line = req.lines().next().unwrap_or("");
        let path = line.split(' ').nth(1).unwrap_or("");
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
        let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if parsed.get("width").is_some() {
            let place = (
                parsed.get("x").and_then(Value::as_i64).unwrap_or(0),
                parsed.get("y").and_then(Value::as_i64).unwrap_or(0),
                parsed.get("width").and_then(Value::as_u64).unwrap_or(0),
                parsed.get("height").and_then(Value::as_u64).unwrap_or(0),
            );
            slot.lock().unwrap_or_else(PoisonError::into_inner).place = Some(place);
        }
        let place = slot.lock().unwrap_or_else(PoisonError::into_inner).place;
        if path == "/selection" {
            let msg = json!({"ok": true, "imageSize": "1K"}).to_string();
            return write_mock(sock, 200, "application/json", &[], msg.as_bytes());
        }
        let Some((x, y, w, h)) = place else {
            let msg = json!({"error": "no retouch result yet; enter a prompt and send it"}).to_string();
            return write_mock(sock, 404, "application/json", &[], msg.as_bytes());
        };
        if path == "/v1/edit" || path == "/generate" || path == "/result" {
            return write_png(sock, result_png, x, y, w, h);
        }
        let msg = json!({"error": "not found"}).to_string();
        write_mock(sock, 404, "application/json", &[], msg.as_bytes())
    }

    fn write_png(sock: &mut TcpStream, png: &[u8], x: i64, y: i64, w: u64, h: u64) -> std::io::Result<()> {
        let headers = [
            ("X-Origin-X", x.to_string()),
            ("X-Origin-Y", y.to_string()),
            ("X-Origin-Width", w.to_string()),
            ("X-Origin-Height", h.to_string()),
            ("X-Image-Size", "1K".into()),
        ];
        let owned: Vec<(String, String)> = headers.into_iter().map(|(k, v)| (k.into(), v)).collect();
        let refs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        write_mock(sock, 200, "image/png", &refs, png)
    }

    fn write_mock(sock: &mut TcpStream, status: u16, content_type: &str, headers: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (k, v) in headers {
            head.push_str(k);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        sock.write_all(head.as_bytes())?;
        sock.write_all(body)?;
        Ok(())
    }

    fn session(depth: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16, "depth": depth})).unwrap();
        s
    }

    fn layer_px(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let doc = &s.active().unwrap().doc;
        let layer = doc.layers.iter().rev().find(|l| l.name.starts_with("Generative Retouch")).unwrap();
        let surf = layer.surface().unwrap();
        [surf.sample_channel(x, y, 0), surf.sample_channel(x, y, 1), surf.sample_channel(x, y, 2), surf.sample_channel(x, y, 3)]
    }

    #[test]
    fn square_is_required_and_bad_params_do_not_panic() {
        let mut s = Session::new();
        assert!(s.execute(SEND, json!({})).is_err());
        let mut s = session(8);
        assert!(!s.is_enabled(SEND));
        s.execute("select.rect", json!({"x": 1, "y": 1, "width": 6, "height": 4})).unwrap();
        assert!(s.execute(SEND, json!({})).is_err(), "a rectangle is not a square");
        s.execute("select.rect", json!({"x": 1, "y": 2, "width": 4, "height": 4})).unwrap();
        assert!(s.is_enabled(SEND));
        // One pixel of rounding (a 1:1 drag that did not land on a pixel boundary) still counts.
        s.execute("select.rect", json!({"x": 1, "y": 1, "width": 5, "height": 4})).unwrap();
        assert!(s.is_enabled(SEND), "a selection one pixel off a square stays enabled");
        assert_eq!(square(&s.active().unwrap().doc).unwrap().size(), photocraft_geom::Size::new(4, 4));
        assert!(s.execute(SEND, json!({"prompt": 1, "url": "http://127.0.0.1:9", "open": false})).is_err());
        assert!(s.execute(SEND, json!({"url": false, "open": false})).is_err());
        assert!(s.execute(SEND, json!({"url": "https://example.com", "open": false})).is_err());
        assert!(s.execute(PLACE, json!({"url": "not a url"})).is_err());
        assert_eq!(s.active().unwrap().doc.layers.len(), 1, "nothing was added");
    }

    #[test]
    fn prompt_places_a_layer_scaled_onto_the_square() {
        let backend = mock(solid_png(2, 2, [0, 255, 0]));
        let mut s = session(8);
        s.execute("select.rect", json!({"x": 2, "y": 3, "width": 4, "height": 4})).unwrap();
        let r = s.execute(SEND, json!({"url": backend.addr, "prompt": "remove stray hair", "open": false})).unwrap();
        assert_eq!(r["width"], 4);
        assert_eq!(r["height"], 4);
        assert_eq!(r["imageSize"], "1K");
        let px = layer_px(&s, 2, 3);
        assert!(px[1] > 0.9 && px[0] < 0.1 && px[3] > 0.9, "placed pixel {px:?}");
        let corner = layer_px(&s, 5, 6);
        assert!(corner[1] > 0.9 && corner[3] > 0.9, "scaled to the whole square {corner:?}");
        let outside = layer_px(&s, 0, 0);
        assert!(outside[3] < 0.05, "outside the selection stays empty {outside:?}");
        assert_eq!(s.active().unwrap().doc.layers.len(), 2);
    }

    #[test]
    fn a_generated_layer_does_not_block_another_generate() {
        let backend = mock(solid_png(2, 2, [0, 0, 255]));
        let mut s = session(8);
        s.execute("select.rect", json!({"x": 2, "y": 3, "width": 4, "height": 4})).unwrap();
        s.execute(SEND, json!({"url": backend.addr, "prompt": "remove stray hair", "open": false})).unwrap();
        assert!(s.is_enabled(SEND), "Edit › Generative Retouch stays available");
        assert!(s.is_enabled(PLACE));
        let again = s.execute(SEND, json!({"url": backend.addr, "prompt": "retouch skin", "open": false})).unwrap();
        assert!(again["layer"].as_u64().is_some());
        let names: Vec<_> = s
            .active()
            .unwrap()
            .doc
            .layers
            .iter()
            .map(|l| l.name.as_str())
            .filter(|n| n.starts_with("Generative Retouch"))
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn merging_the_generated_layer_does_not_block_another_generate() {
        let backend = mock(solid_png(2, 2, [255, 0, 0]));
        let mut s = session(8);
        s.execute("select.rect", json!({"x": 2, "y": 3, "width": 4, "height": 4})).unwrap();
        s.execute(SEND, json!({"url": backend.addr, "prompt": "remove stray hair", "open": false})).unwrap();
        s.execute("layer.mergeDown", json!({})).unwrap();
        let doc = &s.active().unwrap().doc;
        assert_eq!(doc.layers.len(), 1, "the generated layer is merged into the original");
        assert!(doc.selection.is_some(), "the square selection survives the merge");
        assert!(s.is_enabled(SEND), "Edit › Generative Retouch stays available");
        let again = s.execute(SEND, json!({"url": backend.addr, "prompt": "retouch skin", "open": false})).unwrap();
        assert!(again["layer"].as_u64().is_some());
        assert_eq!(s.active().unwrap().doc.layers.len(), 2);
    }

    #[test]
    fn prompt_page_then_place_works_at_16_bit() {
        let backend = mock(solid_png(4, 4, [0, 255, 0]));
        let mut s = session(16);
        s.execute("select.rect", json!({"x": 1, "y": 1, "width": 4, "height": 4})).unwrap();
        let uploaded = s.execute(SEND, json!({"url": backend.addr, "open": false})).unwrap();
        assert_eq!(uploaded["awaitingPrompt"], true);
        assert!(uploaded["page"].as_str().unwrap_or("").starts_with("http://127.0.0.1:"));
        assert_eq!(s.active().unwrap().doc.layers.len(), 1, "uploading does not add a layer");
        // The page's Send is a POST /generate. The mock answers it the same way the backend does.
        let mut sock = TcpStream::connect(backend.addr.trim_start_matches("http://")).unwrap();
        let body = r#"{"prompt":"retouch skin"}"#;
        let req = format!("POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        sock.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).unwrap();
        assert!(buf.windows(12).any(|w| w == b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&buf));
        let placed = s.execute(PLACE, json!({"url": backend.addr})).unwrap();
        assert!(placed["layer"].as_u64().is_some());
        let px = layer_px(&s, 1, 1);
        assert!(px[1] > 0.9 && px[3] > 0.9, "{px:?}");
        let fmt = s.active().unwrap().doc.layers.iter().rev().find(|l| l.name.starts_with("Generative")).unwrap().surface().unwrap().format();
        assert_eq!(fmt.sample, photocraft_color::SampleType::U16);
    }
}
