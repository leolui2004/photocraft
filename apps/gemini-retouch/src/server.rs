//! The local HTTP server. PhotoCraft uploads a PNG whose shape matches a Gemini aspect ratio;
//! the browser page on `/` is where the ratio and the prompt are chosen. The finished image is
//! fetched back by PhotoCraft.

use base64::Engine as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use crate::gemini::{self, Gemini};
use crate::key::KeyStore;
use crate::lang::Lang;

const MAX_BODY: usize = 12 * 1024 * 1024;
const MAX_PROMPT: usize = 4_000;

/// Called once, with the prompt-page URL, after the first selection is stored.
/// Installed before the accept loop so a fast upload cannot miss it.
pub type FirstSelectionHook = Arc<dyn Fn(String) + Send + Sync>;

struct OpenOnce {
    inner: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl OpenOnce {
    fn fire(&self) {
        let hook = self.inner.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[derive(Clone)]
struct Selection {
    png: Vec<u8>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    /// Gemini ratio label that matches `width`×`height` (`16:9`, `1:1`, …).
    ratio: String,
    /// Output tier for this selection (`1K`, `2K` or `4K`).
    image_size: String,
}

#[derive(Clone)]
struct Finished {
    png: Vec<u8>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    image_size: String,
}

struct State {
    selection: Option<Selection>,
    result: Option<Finished>,
    /// Bumps each time a result is stored. 0 means there is no result.
    result_revision: u64,
    /// The image the page confirmed with Use this. Kept when a later generate replaces `result`.
    accepted: Option<Finished>,
    /// `result_revision` that `accepted` was copied from.
    accepted_result: u64,
    /// Bumps once per new confirmation. A repeat Use this on the same result does not bump it.
    accept_seq: u64,
    /// The `accept_seq` PhotoCraft has already placed.
    placed: u64,
    generating: bool,
    error: Option<String>,
    /// Bumps each time a selection is stored, so an open page can replace its preview.
    revision: u64,
}

impl State {
    fn status(&self) -> Value {
        let state = if self.generating {
            "generating"
        } else if self.error.is_some() {
            "error"
        } else if self.result.is_some() {
            "ready"
        } else if self.selection.is_some() {
            "waiting"
        } else {
            "empty"
        };
        let (width, height, x, y) = self.selection.as_ref().map(|s| (s.width, s.height, s.x, s.y)).unwrap_or((0, 0, 0, 0));
        let aspect_ratio = self.selection.as_ref().map(|s| s.ratio.as_str());
        let image_size = self.selection.as_ref().map(|s| s.image_size.as_str());
        json!({
            "state": state,
            "error": self.error,
            "width": width,
            "height": height,
            "x": x,
            "y": y,
            "aspectRatio": aspect_ratio,
            "imageSize": image_size,
            "revision": self.revision,
            "resultRevision": self.result_revision,
            "acceptedResult": self.accepted_result,
            "accept": self.accept_seq,
            "placed": self.placed,
        })
    }
}

/// A running server. Dropping it stops the accept loop.
pub struct Server {
    pub addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bind `addr` and serve until the returned value is dropped.
pub fn start(
    addr: SocketAddr,
    gemini: Arc<dyn Gemini>,
    keys: Arc<KeyStore>,
    lang: Lang,
    on_first_selection: Option<FirstSelectionHook>,
) -> Result<Server, String> {
    let listener = TcpListener::bind(addr).map_err(|e| lang.err_listen(&addr, &e))?;
    listener.set_nonblocking(false).map_err(|e| lang.err_listen(&addr, &e))?;
    let bound = listener.local_addr().map_err(|e| lang.err_listen(&addr, &e))?;
    let page = format!("{}{}", crate::http_origin(bound), lang.page_code());
    let open_page =
        Arc::new(OpenOnce { inner: Mutex::new(on_first_selection.map(|hook| Arc::new(move || hook(page.clone())) as Arc<dyn Fn() + Send + Sync>)) });
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let thread = std::thread::Builder::new()
        .name("retouch-http".into())
        .spawn(move || accept_loop(listener, gemini, keys, flag, lang, open_page))
        .map_err(|e| lang.err_thread(&e))?;
    Ok(Server { addr: bound, shutdown, thread: Some(thread) })
}

fn accept_loop(listener: TcpListener, gemini: Arc<dyn Gemini>, keys: Arc<KeyStore>, shutdown: Arc<AtomicBool>, lang: Lang, open_page: Arc<OpenOnce>) {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let (sock, _) = match listener.accept() {
            Ok(v) => v,
            Err(e) => {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                eprintln!("{}", lang.log_accept_failed(&e));
                continue;
            }
        };
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let gemini = Arc::clone(&gemini);
        let keys = Arc::clone(&keys);
        let req_lang = lang;
        let open_page = Arc::clone(&open_page);
        let _ = std::thread::Builder::new().name("retouch-req".into()).spawn(move || {
            if let Err(e) = handle(sock, gemini.as_ref(), keys.as_ref(), req_lang, &open_page) {
                let kind = e.kind();
                if kind != std::io::ErrorKind::UnexpectedEof && kind != std::io::ErrorKind::ConnectionAborted && kind != std::io::ErrorKind::ConnectionReset {
                    eprintln!("{}", req_lang.log_backend(&e));
                }
            }
        });
    }
}

fn handle(mut sock: TcpStream, gemini: &dyn Gemini, keys: &KeyStore, lang: Lang, open_page: &OpenOnce) -> std::io::Result<()> {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(30)));
    let req = match read_request(&mut sock) {
        Ok(r) => r,
        Err(e) => {
            let _ = write_response(&mut sock, 400, "text/plain; charset=utf-8", &[], e.to_string().as_bytes());
            return Ok(());
        }
    };
    let response = route(&req, gemini, keys, lang, open_page);
    write_response(&mut sock, response.status, &response.content_type, &response.headers, &response.body)
}

struct Incoming {
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

struct Outgoing {
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn route(req: &Incoming, gemini: &dyn Gemini, keys: &KeyStore, lang: Lang, open_page: &OpenOnce) -> Outgoing {
    if req.method == "GET" {
        if req.path == "/" {
            return html(&page_html(lang));
        }
        if let Some(asked) = path_lang(&req.path) {
            return html(&page_html(asked));
        }
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/key-status") => json_out(200, &json!({"set": keys.has_key()})),
        ("POST", "/api-key") => save_key(req, keys),
        ("GET", "/status") => json_out(200, &state().status()),
        ("GET", "/selection.png") => selection_png(),
        ("GET", "/result") => result_png(),
        ("GET", "/accepted") => accepted_png(),
        ("POST", "/selection") => store_selection(&req.body, open_page),
        ("POST", "/generate") => generate(req, gemini),
        ("POST", "/confirm") => confirm_result(),
        ("POST", "/placed") => mark_placed(),
        ("POST", "/v1/edit") => edit_once(&req.body, gemini),
        _ => text(404, "not found"),
    }
}

fn state() -> std::sync::MutexGuard<'static, State> {
    app_state().lock().unwrap_or_else(PoisonError::into_inner)
}

fn app_state() -> &'static Mutex<State> {
    use std::sync::OnceLock;
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            selection: None,
            result: None,
            result_revision: 0,
            accepted: None,
            accepted_result: 0,
            accept_seq: 0,
            placed: 0,
            generating: false,
            error: None,
            revision: 0,
        })
    })
}

fn html(page: &str) -> Outgoing {
    Outgoing { status: 200, content_type: "text/html; charset=utf-8".into(), headers: Vec::new(), body: page.as_bytes().to_vec() }
}

fn text(status: u16, msg: &str) -> Outgoing {
    json_out(status, &json!({"error": msg}))
}

fn json_out(status: u16, v: &Value) -> Outgoing {
    Outgoing { status, content_type: "application/json".into(), headers: Vec::new(), body: v.to_string().into_bytes() }
}

fn png_out(png: &[u8], finished: &Finished) -> Outgoing {
    // Gemini returns JPEG. PhotoCraft decodes either; the type tells the browser which it is.
    let content_type = if png.len() >= 2 && png.first() == Some(&0xff) && png.get(1) == Some(&0xd8) { "image/jpeg" } else { "image/png" };
    Outgoing {
        status: 200,
        content_type: content_type.into(),
        headers: vec![
            ("X-Origin-X".into(), finished.x.to_string()),
            ("X-Origin-Y".into(), finished.y.to_string()),
            ("X-Origin-Width".into(), finished.width.to_string()),
            ("X-Origin-Height".into(), finished.height.to_string()),
            ("X-Image-Size".into(), finished.image_size.clone()),
        ],
        body: png.to_vec(),
    }
}

fn selection_png() -> Outgoing {
    let g = state();
    match &g.selection {
        Some(s) => Outgoing { status: 200, content_type: "image/png".into(), headers: Vec::new(), body: s.png.clone() },
        None => text(404, "no selection has been sent yet"),
    }
}

fn result_png() -> Outgoing {
    let g = state();
    if g.generating {
        return text(409, "the retouch is still running");
    }
    match &g.result {
        Some(f) => png_out(&f.png, f),
        None => text(404, "no retouch result yet; enter a prompt and send it"),
    }
}

fn accepted_png() -> Outgoing {
    let g = state();
    match &g.accepted {
        Some(f) => png_out(&f.png, f),
        None => text(404, "no image has been confirmed yet"),
    }
}

/// The page's Use this. Copies the current result so a later regenerate cannot replace it
/// before PhotoCraft places it. Pressing it again on the same result does not bump the counter.
fn confirm_result() -> Outgoing {
    let mut g = state();
    if g.generating {
        return text(409, "the retouch is still running");
    }
    let Some(result) = g.result.clone() else {
        return text(400, "no retouch result yet; enter a prompt and send it");
    };
    let revision = g.result_revision;
    if g.accepted_result == revision && g.accept_seq > 0 {
        return json_out(200, &json!({"ok": true, "accept": g.accept_seq, "resultRevision": revision}));
    }
    g.accepted = Some(result);
    g.accepted_result = revision;
    g.accept_seq = g.accept_seq.saturating_add(1);
    let seq = g.accept_seq;
    json_out(200, &json!({"ok": true, "accept": seq, "resultRevision": revision}))
}

fn mark_placed() -> Outgoing {
    let mut g = state();
    if g.accept_seq > 0 {
        g.placed = g.accept_seq;
    }
    json_out(200, &json!({"ok": true, "placed": g.placed}))
}

fn save_key(req: &Incoming, keys: &KeyStore) -> Outgoing {
    let raw = if req.content_type.starts_with("application/json") {
        match serde_json::from_slice::<Value>(&req.body) {
            Ok(v) => v.get("key").and_then(Value::as_str).unwrap_or("").to_string(),
            Err(_) => return text(400, "the body must be JSON"),
        }
    } else {
        form_value(&req.body, "key").unwrap_or_default()
    };
    match keys.set(&raw) {
        Ok(()) => json_out(200, &json!({"ok": true})),
        Err(e) => text(400, &e),
    }
}

fn store_selection(body: &[u8], open_page: &OpenOnce) -> Outgoing {
    let parsed = match parse_selection(body) {
        Ok(s) => s,
        Err(e) => return text(400, &e),
    };
    let image_size = parsed.image_size.clone();
    let width = parsed.width;
    let height = parsed.height;
    let aspect_ratio = parsed.ratio.clone();
    {
        let mut g = state();
        g.error = None;
        g.result = None;
        g.result_revision = 0;
        g.revision = g.revision.saturating_add(1);
        g.selection = Some(parsed);
    }
    // The selection is visible to other requests before the browser is asked to open.
    open_page.fire();
    json_out(200, &json!({"ok": true, "width": width, "height": height, "aspectRatio": aspect_ratio, "imageSize": image_size}))
}

fn parse_selection(body: &[u8]) -> Result<Selection, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "the body must be JSON".to_string())?;
    let png_b64 = v.get("png").and_then(Value::as_str).ok_or("missing `png` (base64)")?;
    let png = decode_b64(png_b64)?;
    if png.len() > 8 * 1024 * 1024 {
        return Err("the selection PNG is larger than 8 MB".into());
    }
    let (iw, ih) = gemini::png_size(&png)?;
    let width = number(&v, "width")?;
    let height = number(&v, "height")?;
    if width != iw || height != ih {
        return Err(format!("width/height {width}×{height} do not match the PNG ({iw}×{ih})"));
    }
    let prepared = gemini::prepare(width, height)?;
    let x = signed(&v, "x")?;
    let y = signed(&v, "y")?;
    Ok(Selection { png, x, y, width, height, ratio: prepared.ratio.to_string(), image_size: prepared.image_size.to_string() })
}

fn generate(req: &Incoming, gemini: &dyn Gemini) -> Outgoing {
    let prompt = match prompt_from(req) {
        Ok(p) => p,
        Err(e) => return text(400, &e),
    };
    let aspect = match aspect_override(req) {
        Ok(r) => r,
        Err(e) => return text(400, &e),
    };
    let selection = {
        let mut g = state();
        if g.generating {
            return text(409, "a retouch is already running");
        }
        let Some(sel) = g.selection.clone() else {
            return text(400, "no selection yet; send a selection from PhotoCraft first");
        };
        g.generating = true;
        g.error = None;
        sel
    };
    let ratio = aspect.unwrap_or(selection.ratio.clone());
    finish_edit(&selection, &ratio, &prompt, gemini)
}

fn edit_once(body: &[u8], gemini: &dyn Gemini) -> Outgoing {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return text(400, "the body must be JSON"),
    };
    let prompt = match v.get("prompt").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) if p.chars().count() <= MAX_PROMPT => p.to_string(),
        Some(_) => return text(400, "the prompt is too long"),
        None => return text(400, "missing `prompt`"),
    };
    let selection = match parse_selection(body) {
        Ok(s) => s,
        Err(e) => return text(400, &e),
    };
    let aspect = match v.get("aspectRatio") {
        None => None,
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if gemini::find_ratio(s).is_some() {
                Some(s.to_string())
            } else {
                return text(400, &gemini::unknown_ratio());
            }
        }
        Some(_) => return text(400, "`aspectRatio` must be a string"),
    };
    {
        let mut g = state();
        if g.generating {
            return text(409, "a retouch is already running");
        }
        g.generating = true;
        g.error = None;
        g.revision = g.revision.saturating_add(1);
        g.selection = Some(selection.clone());
    }
    let ratio = aspect.unwrap_or(selection.ratio.clone());
    finish_edit(&selection, &ratio, &prompt, gemini)
}

fn aspect_override(req: &Incoming) -> Result<Option<String>, String> {
    if !req.content_type.starts_with("application/json") {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(&req.body).map_err(|_| "the body must be JSON".to_string())?;
    match v.get("aspectRatio") {
        None => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                Ok(None)
            } else if gemini::find_ratio(s).is_some() {
                Ok(Some(s.to_string()))
            } else {
                Err(gemini::unknown_ratio())
            }
        }
        Some(_) => Err("`aspectRatio` must be a string".into()),
    }
}

fn finish_edit(selection: &Selection, ratio_label: &str, prompt: &str, gemini: &dyn Gemini) -> Outgoing {
    let Some(ratio) = gemini::find_ratio(ratio_label) else {
        let mut g = state();
        g.generating = false;
        return text(400, &gemini::unknown_ratio());
    };
    let (x, y, width, height) = if ratio.label == selection.ratio {
        (selection.x, selection.y, selection.width, selection.height)
    } else {
        gemini::inscribe(selection.x, selection.y, selection.width, selection.height, ratio)
    };
    let image_size = match gemini::image_size_for(width, height, ratio) {
        Ok(s) => s.to_string(),
        Err(e) => {
            let mut g = state();
            g.generating = false;
            g.error = Some(e.clone());
            return text(400, &e);
        }
    };
    let edited = gemini.edit(&selection.png, ratio.label, &image_size, prompt);
    let mut g = state();
    g.generating = false;
    match edited {
        Ok(out) => {
            let finished = Finished { png: out, x, y, width, height, image_size };
            let response = png_out(&finished.png, &finished);
            g.result_revision = g.result_revision.saturating_add(1);
            g.result = Some(finished);
            g.error = None;
            response
        }
        Err(e) => {
            g.error = Some(e.clone());
            text(502, &e)
        }
    }
}

fn prompt_from(req: &Incoming) -> Result<String, String> {
    let raw = if req.content_type.starts_with("application/json") {
        let v: Value = serde_json::from_slice(&req.body).map_err(|_| "the body must be JSON".to_string())?;
        v.get("prompt").and_then(Value::as_str).unwrap_or("").to_string()
    } else {
        form_value(&req.body, "prompt").unwrap_or_default()
    };
    let prompt = raw.trim().to_string();
    if prompt.is_empty() {
        return Err("enter a prompt".into());
    }
    if prompt.chars().count() > MAX_PROMPT {
        return Err("the prompt is too long".into());
    }
    Ok(prompt)
}

fn form_value(body: &[u8], key: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    for pair in text.split('&') {
        let (k, v) = pair.split_once('=')?;
        if url_decode(k) == key {
            return Some(url_decode(v));
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(h) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn number(v: &Value, key: &str) -> Result<u32, String> {
    let n = v.get(key).and_then(Value::as_u64).ok_or_else(|| format!("missing `{key}`"))?;
    u32::try_from(n).map_err(|_| format!("`{key}` is out of range"))
}

fn signed(v: &Value, key: &str) -> Result<i32, String> {
    let n = v.get(key).and_then(Value::as_i64).ok_or_else(|| format!("missing `{key}`"))?;
    i32::try_from(n).map_err(|_| format!("`{key}` is out of range"))
}

fn decode_b64(data: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(cleaned.as_bytes()))
        .map_err(|_| "png is not valid base64".to_string())
}

struct RequestError(String);

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for RequestError {
    fn from(e: std::io::Error) -> Self {
        RequestError(e.to_string())
    }
}

fn read_request(sock: &mut TcpStream) -> Result<Incoming, RequestError> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        if buf.len() > 64 * 1024 {
            return Err(RequestError("the request headers are too large".into()));
        }
        let n = sock.read(&mut tmp)?;
        if n == 0 {
            return Err(RequestError("connection closed".into()));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = find_header_end(&buf) {
            break i;
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| RequestError("headers are not UTF-8".into()))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    if method.is_empty() || path.is_empty() {
        return Err(RequestError("bad request line".into()));
    }
    let mut content_type = String::new();
    let mut length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "content-type" {
            content_type = value.to_string();
        } else if name == "content-length" {
            length = value.parse().map_err(|_| RequestError("bad Content-Length".into()))?;
        }
    }
    if length > MAX_BODY {
        return Err(RequestError("the request body is too large".into()));
    }
    let mut body = buf.split_off(header_end + 4);
    while body.len() < length {
        let n = sock.read(&mut tmp)?;
        if n == 0 {
            return Err(RequestError("connection closed before the body finished".into()));
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(length);
    Ok(Incoming { method, path, content_type, body })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_response(sock: &mut TcpStream, status: u16, content_type: &str, headers: &[(String, String)], body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        body.len()
    );
    for (name, value) in headers {
        if !value.chars().any(|c| c == '\r' || c == '\n') {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
    }
    head.push_str("\r\n");
    sock.write_all(head.as_bytes())?;
    sock.write_all(body)?;
    Ok(())
}

/// `/ja` and `/zh-Hant` select the prompt page language. Anything else stays a normal path.
fn path_lang(path: &str) -> Option<Lang> {
    let code = path.strip_prefix('/').unwrap_or(path);
    if code.is_empty() || code.contains('/') {
        return None;
    }
    Lang::from_code(code)
}

fn page_html(lang: Lang) -> String {
    let text = lang.page();
    let i18n = json!({
        "htmlLang": text.html_lang,
        "title": text.title,
        "intro": text.intro,
        "keyBefore": text.key_before,
        "keyLink": text.key_link,
        "keyAfter": text.key_after,
        "apiKey": text.api_key,
        "saveKey": text.save_key,
        "empty": text.empty,
        "promptHint": text.prompt_hint,
        "hair": text.hair,
        "skin": text.skin,
        "passerby": text.passerby,
        "ratio": text.ratio,
        "ratios": gemini::ratio_labels(),
        "send": text.send,
        "use": text.use_this,
        "again": text.again,
        "showOriginal": text.show_original,
        "showResult": text.show_result,
        "adding": text.adding,
        "added": text.added,
        "confirmFailed": text.confirm_failed,
        "menuFallback": text.menu_fallback,
        "resultAlt": text.result_alt,
        "saving": text.saving,
        "saveFailed": text.save_failed,
        "saved": text.saved,
        "unreachable": text.unreachable,
        "alt": text.alt,
        "sending": text.sending,
        "ready": text.ready,
        "failed": text.failed,
        "received": text.received,
        "needKey": text.need_key,
    });
    // Keep `<` out of the script so a translation cannot close the script element.
    let i18n = i18n.to_string().replace('<', "\\u003c");
    PAGE_TEMPLATE.replace("%%HTML_LANG%%", text.html_lang).replace("%%TITLE%%", &escape_html(text.title)).replace("%%I18N%%", &i18n)
}

fn escape_html(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

const PAGE_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="%%HTML_LANG%%">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>%%TITLE%%</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 15px/1.45 system-ui, sans-serif; margin: 0; background: #1c1c1c; color: #eee; }
  main { max-width: 640px; margin: 0 auto; padding: 28px 20px 48px; }
  h1 { font-size: 20px; font-weight: 600; margin: 0 0 8px; }
  p { margin: 0 0 14px; color: #bbb; }
  img { display: block; max-width: 100%; background: #111; border: 1px solid #333; }
  .empty { padding: 28px; text-align: center; color: #888; border: 1px dashed #444; }
  textarea { width: 100%; min-height: 88px; box-sizing: border-box; margin: 14px 0 8px; padding: 10px; font: inherit; background: #111; color: inherit; border: 1px solid #444; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  button { font: inherit; padding: 8px 14px; border: 1px solid #555; background: #2a2a2a; color: inherit; cursor: pointer; }
  button.primary { background: #3d6df2; border-color: #3d6df2; color: white; }
  button:disabled { opacity: 0.5; cursor: default; }
  input, select { font: inherit; padding: 8px 10px; background: #111; color: inherit; border: 1px solid #444; }
  input { flex: 1; min-width: 12em; }
  label { color: #bbb; }
  #keybox { margin: 0 0 18px; padding: 12px 14px; border: 1px solid #444; }
  a { color: #9eb6ff; }
  #decide { margin-top: 10px; }
  #status { margin-top: 14px; min-height: 1.4em; }
  .err { color: #ff8a80; }
  .ok { color: #9ccc9c; }
</style>
</head>
<body>
<main>
  <h1 id="title"></h1>
  <p id="intro"></p>
  <div id="keybox" hidden>
    <p id="keyhelp"></p>
    <div class="row">
      <input id="apikey" type="password" autocomplete="off" spellcheck="false">
      <button type="button" class="primary" id="savekey"></button>
    </div>
  </div>
  <div id="preview" class="empty"></div>
  <div class="row" id="decide" hidden>
    <button type="button" class="primary" id="use"></button>
    <button type="button" id="again"></button>
    <button type="button" id="flip"></button>
  </div>
  <div class="row" id="ratiorow">
    <label id="ratiolabel" for="ratio"></label>
    <select id="ratio"></select>
  </div>
  <textarea id="prompt"></textarea>
  <div class="row">
    <button type="button" id="hair"></button>
    <button type="button" id="skin"></button>
    <button type="button" id="passerby"></button>
    <button type="button" class="primary" id="send"></button>
  </div>
  <div id="status"></div>
</main>
<script>
const T = %%I18N%%;
document.documentElement.lang = T.htmlLang;
document.title = T.title;
document.getElementById("title").textContent = T.title;
document.getElementById("intro").textContent = T.intro;
const keyhelp = document.getElementById("keyhelp");
keyhelp.append(T.keyBefore);
const keylink = document.createElement("a");
keylink.href = "https://aistudio.google.com/apikey";
keylink.textContent = T.keyLink;
keyhelp.append(keylink, T.keyAfter);
document.getElementById("apikey").placeholder = T.apiKey;
document.getElementById("savekey").textContent = T.saveKey;
document.getElementById("preview").textContent = T.empty;
document.getElementById("prompt").placeholder = T.promptHint;
document.getElementById("hair").textContent = T.hair;
document.getElementById("skin").textContent = T.skin;
document.getElementById("passerby").textContent = T.passerby;
document.getElementById("ratiolabel").textContent = T.ratio;
const ratio = document.getElementById("ratio");
for (const name of T.ratios) {
  const opt = document.createElement("option");
  opt.value = name;
  opt.textContent = name;
  ratio.append(opt);
}
document.getElementById("send").textContent = T.send;
document.getElementById("use").textContent = T.use;
document.getElementById("again").textContent = T.again;
const prompt = document.getElementById("prompt");
const status = document.getElementById("status");
const preview = document.getElementById("preview");
const send = document.getElementById("send");
const useBtn = document.getElementById("use");
const again = document.getElementById("again");
const flip = document.getElementById("flip");
const decide = document.getElementById("decide");
const keybox = document.getElementById("keybox");
const apikey = document.getElementById("apikey");
let keySet = false;
let shownRevision = "";
let shownResult = "";
let view = "auto";
let confirmAt = 0;
let ratioRevision = "";
document.getElementById("hair").onclick = () => { prompt.value = T.hair; };
document.getElementById("skin").onclick = () => { prompt.value = T.skin; };
document.getElementById("passerby").onclick = () => { prompt.value = T.passerby; };
document.getElementById("savekey").onclick = async () => {
  note(T.saving);
  try {
    const res = await fetch("/api-key", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key: apikey.value }) });
    const msg = await res.json();
    if (!res.ok) { note(msg.error || T.saveFailed, "err"); return; }
    apikey.value = "";
    note(T.saved, "ok");
    refresh();
  } catch (e) {
    note(T.unreachable, "err");
  }
};
function note(text, cls) { status.textContent = text; status.className = cls || ""; }
function fill(template, info) {
  return template.split("{w}").join(info.width).split("{h}").join(info.height).split("{size}").join(info.imageSize).split("{ratio}").join(info.aspectRatio || "");
}
function syncRatio(info) {
  if (!info.aspectRatio) return;
  const rev = String(info.revision || 0);
  if (ratioRevision === rev) return;
  ratioRevision = rev;
  ratio.value = info.aspectRatio;
}
function showSelection(info) {
  const live = info.state === "waiting" || info.state === "ready" || info.state === "generating" || info.state === "error";
  if (!live) return;
  const rev = String(info.revision);
  const existing = preview.querySelector("img");
  if (existing && existing.dataset.rev === rev && shownRevision === rev) return;
  shownRevision = rev;
  const img = document.createElement("img");
  img.alt = T.alt;
  img.dataset.rev = rev;
  const src = () => "/selection.png?rev=" + encodeURIComponent(rev) + "&t=" + Date.now();
  img.onerror = () => {
    const n = Number(img.dataset.tries || "0") + 1;
    if (n > 4) return;
    img.dataset.tries = String(n);
    setTimeout(() => { img.src = src(); }, 200);
  };
  img.src = src();
  preview.className = "";
  preview.replaceChildren(img);
}
function showResult(info) {
  const rev = String(info.resultRevision || 0);
  if (rev === "0") return;
  const existing = preview.querySelector("img");
  if (existing && existing.dataset.result === rev && shownResult === rev) return;
  shownResult = rev;
  shownRevision = "";
  const img = document.createElement("img");
  img.alt = T.resultAlt;
  img.dataset.result = rev;
  img.src = "/result?rev=" + encodeURIComponent(rev) + "&t=" + Date.now();
  preview.className = "";
  preview.replaceChildren(img);
}
function showPreview(info) {
  const rev = info.resultRevision || 0;
  const hasResult = rev > 0;
  if (hasResult && info.state === "ready" && view === "auto") showResult(info);
  else if (view === "result" && info.state === "ready") showResult(info);
  else if (info.state === "generating" && shownResult) { /* keep the image already on screen */ }
  else showSelection(info);
  const showingResult = hasResult && view !== "source" && info.state === "ready";
  decide.hidden = !hasResult;
  flip.textContent = showingResult ? T.showOriginal : T.showResult;
  const confirmed = hasResult && info.acceptedResult === rev && info.accept > 0;
  useBtn.disabled = !keySet || info.state !== "ready" || confirmed;
  again.disabled = !keySet || info.state === "generating" || info.state === "empty" || !prompt.value.trim();
}
function noteReady(info) {
  const rev = info.resultRevision || 0;
  const confirmed = rev > 0 && info.acceptedResult === rev && info.accept > 0;
  if (!confirmed) { note(T.ready, "ok"); return; }
  if (info.placed >= info.accept) { note(T.added, "ok"); return; }
  if (confirmAt && Date.now() - confirmAt < 4000) { note(T.adding); return; }
  note(T.menuFallback);
}
async function refresh() {
  let info;
  try { info = await (await fetch("/status", { cache: "no-store" })).json(); }
  catch (e) { note(T.unreachable, "err"); return; }
  try {
    const keyInfo = await (await fetch("/key-status", { cache: "no-store" })).json();
    keySet = !!keyInfo.set;
    keybox.hidden = keySet;
  } catch (e) { /* the prompt page still works; Send will say if the key is missing */ }
  syncRatio(info);
  showPreview(info);
  ratio.disabled = info.state === "generating" || info.state === "empty";
  if (info.state === "generating") note(T.sending);
  else if (info.state === "ready") noteReady(info);
  else if (info.state === "error") note(info.error || T.failed, "err");
  else if (info.state === "waiting") note(fill(T.received, info));
  else if (!keySet) note(T.needKey);
  else if (status.className !== "ok") note("");
  send.disabled = !keySet || info.state === "generating" || info.state === "empty";
}
flip.onclick = () => {
  view = view === "source" ? "result" : "source";
  if (view === "result") shownResult = "";
  else shownRevision = "";
  refresh();
};
async function submit() {
  if (!keySet || !prompt.value.trim()) return;
  send.disabled = true;
  useBtn.disabled = true;
  again.disabled = true;
  view = "auto";
  note(T.sending);
  try {
    const body = { prompt: prompt.value };
    if (ratioRevision) body.aspectRatio = ratio.value;
    const res = await fetch("/generate", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
    const kind = res.headers.get("content-type") || "";
    if (!res.ok) {
      const msg = kind.includes("json") ? (await res.json()).error : await res.text();
      note(msg || T.failed, "err");
    } else {
      note(T.ready, "ok");
    }
  } catch (e) {
    note(T.unreachable, "err");
  }
  refresh();
}
send.onclick = submit;
again.onclick = submit;
useBtn.onclick = async () => {
  useBtn.disabled = true;
  confirmAt = Date.now();
  note(T.adding);
  try {
    const res = await fetch("/confirm", { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    const kind = res.headers.get("content-type") || "";
    if (!res.ok) {
      const msg = kind.includes("json") ? (await res.json()).error : await res.text();
      note(msg || T.confirmFailed, "err");
      confirmAt = 0;
    }
  } catch (e) {
    note(T.unreachable, "err");
    confirmAt = 0;
  }
  refresh();
};
refresh();
setInterval(refresh, 500);
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use std::io::{Read, Write};
    use std::sync::Mutex;

    /// `State` is process-global. Tests that read it hold this for their whole body.
    fn hold_state() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    struct Fake {
        png: Vec<u8>,
        seen: Mutex<Option<(String, String)>>,
    }

    impl Gemini for Fake {
        fn edit(&self, _png: &[u8], aspect_ratio: &str, _image_size: &str, prompt: &str) -> Result<Vec<u8>, String> {
            *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some((aspect_ratio.to_string(), prompt.to_string()));
            Ok(self.png.clone())
        }
    }

    fn one_pixel() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x5d, 0xec, 0xea, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]
    }

    fn exchange(addr: SocketAddr, req: &str) -> (u16, String, Vec<u8>) {
        let mut sock = TcpStream::connect(addr).unwrap();
        sock.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        let status = text.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
        let body = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| buf[i + 4..].to_vec()).unwrap_or_default();
        (status, text.into_owned(), body)
    }

    #[test]
    fn prompt_page_sends_and_photo_craft_can_fetch_the_image() {
        let _hold = hold_state();
        {
            let mut g = state();
            g.selection = None;
            g.result = None;
            g.result_revision = 0;
            g.accepted = None;
            g.accepted_result = 0;
            g.accept_seq = 0;
            g.placed = 0;
            g.generating = false;
            g.error = None;
            g.revision = 0;
        }
        let png = one_pixel();
        let fake = Arc::new(Fake { png: png.clone(), seen: Mutex::new(None) });
        let keys = Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-unused-key"), Some("present".into())));
        let server = start("127.0.0.1:0".parse().unwrap(), fake.clone(), keys, Lang::En, None).unwrap();
        let addr = server.addr;

        let (status, page, _) = exchange(addr, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{page}");
        assert!(page.contains("PhotoCraft retouch"));
        assert!(page.contains("No selection yet."));

        let (status, ja, _) = exchange(addr, "GET /ja HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{ja}");
        assert!(ja.contains("まだ選択範囲がありません"), "{ja}");
        assert!(!ja.contains("No selection yet."), "{ja}");

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let body = json!({"png": b64, "x": 4, "y": 6, "width": 1, "height": 1}).to_string();
        let posted = format!(
            "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, resp) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        let stored: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(stored["imageSize"], "1K");

        let prompt_body = r#"{"prompt":"remove stray hair"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, body) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        assert_eq!(body, png);
        assert!(head.to_ascii_lowercase().contains("x-origin-x: 4"));
        assert!(head.to_ascii_lowercase().contains("x-origin-y: 6"));
        assert!(head.to_ascii_lowercase().contains("x-image-size: 1k"));
        let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
        assert_eq!(seen, ("1:1".into(), "remove stray hair".into()));

        // A finished result must not disable Send. The page only disables it with no key,
        // while a retouch is running, or before any selection has arrived.
        let (status, head, status_body) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{head}");
        let status_json: Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_json["state"], "ready");
        assert_eq!(status_json["revision"].as_u64(), Some(1));
        let page_src = page_html(Lang::En);
        assert!(page_src.contains(r#"send.disabled = !keySet || info.state === "generating" || info.state === "empty""#));
        assert!(page_src.contains("info.revision"));
        assert!(page_src.contains("\"use\":\"Use this\""), "{page_src}");
        assert!(page_src.contains("\"again\":\"Regenerate\""));
        assert!(page_src.contains("/confirm"));
        assert!(page_src.contains("resultRevision"));
        let ja_page = page_html(Lang::Ja);
        assert!(ja_page.contains("これを使う"), "{ja_page}");
        assert!(ja_page.contains("再生成"));
        assert!(ja_page.contains("ほつれ毛を消す"), "{ja_page}");
        assert!(ja_page.contains("通行人を消す"), "{ja_page}");
        assert!(page_src.contains("\"passerby\":\"Remove passerby\""), "{page_src}");
        assert!(page_src.contains("\"ratio\":\"Aspect ratio\""), "{page_src}");
        assert!(page_src.contains("\"16:9\""), "{page_src}");
        assert!(page_src.contains("aspectRatio"), "{page_src}");
        assert!(ja_page.contains("縦横比"), "{ja_page}");
        assert!(page_src.contains(r#"cache: "no-store""#));
        assert!(head.to_ascii_lowercase().contains("cache-control: no-store"));
        assert_eq!(path_lang("/ja"), Some(Lang::Ja));
        assert_eq!(path_lang("/zh-Hant"), Some(Lang::ZhHant));
        assert_eq!(path_lang("/status"), None);

        let prompt_body = r#"{"prompt":"retouch skin"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, body) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        assert_eq!(body, png);
        let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
        assert_eq!(seen, ("1:1".into(), "retouch skin".into()));

        let (status, _, again) = exchange(addr, "GET /result HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(again, png);

        let bad = r#"{"prompt":"x","png":"aaaa","x":0,"y":0,"width":2,"height":3}"#;
        let posted = format!(
            "POST /v1/edit HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bad}",
            bad.len()
        );
        let (status, _, _) = exchange(addr, &posted);
        assert_eq!(status, 400);
    }

    fn png_wh(w: u32, h: u32) -> Vec<u8> {
        let mut png = one_pixel();
        png[16..20].copy_from_slice(&w.to_be_bytes());
        png[20..24].copy_from_slice(&h.to_be_bytes());
        png
    }

    #[test]
    fn a_wide_selection_keeps_its_ratio_and_can_switch_to_another() {
        let _hold = hold_state();
        {
            let mut g = state();
            g.selection = None;
            g.result = None;
            g.result_revision = 0;
            g.accepted = None;
            g.accepted_result = 0;
            g.accept_seq = 0;
            g.placed = 0;
            g.generating = false;
            g.error = None;
            g.revision = 0;
        }
        let png = png_wh(16, 9);
        let fake = Arc::new(Fake { png: one_pixel(), seen: Mutex::new(None) });
        let keys = Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-ratio"), Some("present".into())));
        let server = start("127.0.0.1:0".parse().unwrap(), fake.clone(), keys, Lang::En, None).unwrap();
        let addr = server.addr;

        let odd = png_wh(4, 9);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&odd);
        let body = json!({"png": b64, "x": 0, "y": 0, "width": 4, "height": 9}).to_string();
        let posted = format!(
            "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 400, "{head}");

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let body = json!({"png": b64, "x": 10, "y": 4, "width": 16, "height": 9}).to_string();
        let posted = format!(
            "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, resp) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        let stored: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(stored["aspectRatio"], "16:9");
        assert_eq!(stored["imageSize"], "1K");

        let prompt_body = r#"{"prompt":"remove passerby"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-origin-x: 10"), "{head}");
        assert!(lower.contains("x-origin-width: 16"), "{head}");
        assert!(lower.contains("x-origin-height: 9"), "{head}");
        let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
        assert_eq!(seen.0, "16:9");

        let prompt_body = r#"{"prompt":"remove passerby","aspectRatio":"1:1"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-origin-x: 13"), "{head}");
        assert!(lower.contains("x-origin-y: 4"), "{head}");
        assert!(lower.contains("x-origin-width: 9"), "{head}");
        assert!(lower.contains("x-origin-height: 9"), "{head}");
        let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
        assert_eq!(seen.0, "1:1");

        let prompt_body = r#"{"prompt":"remove passerby","aspectRatio":"2:1"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, _, _) = exchange(addr, &posted);
        assert_eq!(status, 400);
    }

    #[test]
    fn the_page_saves_an_api_key_without_repeating_it() {
        let dir = std::env::temp_dir().join(format!("photocraft-retouch-http-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gemini-api-key.txt");
        let keys = Arc::new(crate::key::KeyStore::open(path.clone(), Some(String::new())));
        let fake = Arc::new(Fake { png: Vec::new(), seen: Mutex::new(None) });
        let server = start("127.0.0.1:0".parse().unwrap(), fake, Arc::clone(&keys), Lang::En, None).unwrap();

        let (status, page, _) = exchange(server.addr, "GET /key-status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{page}");
        assert!(page.contains("\"set\":false"), "{page}");

        let secret = "AIzaSyExampleKey123";
        let body = format!(r#"{{"key":"{secret}"}}"#);
        let posted = format!(
            "POST /api-key HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, resp) = exchange(server.addr, &posted);
        assert_eq!(status, 200, "{head}");
        let text = String::from_utf8_lossy(&resp);
        assert!(!text.contains(secret), "{text}");
        assert!(text.contains("\"ok\":true"), "{text}");
        assert_eq!(keys.get(), secret);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), secret);

        let spaced = r#"{"key":"not a key"}"#;
        let posted = format!(
            "POST /api-key HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{spaced}",
            spaced.len()
        );
        let (status, _, _) = exchange(server.addr, &posted);
        assert_eq!(status, 400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_selection_opens_the_page_once_and_a_bad_body_does_not() {
        let _hold = hold_state();
        {
            let mut g = state();
            g.selection = None;
            g.result = None;
            g.result_revision = 0;
            g.accepted = None;
            g.accepted_result = 0;
            g.accept_seq = 0;
            g.placed = 0;
            g.generating = false;
            g.error = None;
            g.revision = 0;
        }
        let opened = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&opened);
        let hook: FirstSelectionHook = Arc::new(move |url| {
            record.lock().unwrap_or_else(PoisonError::into_inner).push(url);
        });
        let fake = Arc::new(Fake { png: one_pixel(), seen: Mutex::new(None) });
        let keys = Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-open-once"), Some("present".into())));
        let server = start("127.0.0.1:0".parse().unwrap(), fake, keys, Lang::En, Some(hook)).unwrap();
        let addr = server.addr;

        let posted = "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{";
        let (status, _, _) = exchange(addr, posted);
        assert_eq!(status, 400);
        assert!(opened.lock().unwrap_or_else(PoisonError::into_inner).is_empty());

        let (status, _, before) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        let before: Value = serde_json::from_slice(&before).unwrap();
        let revision = before["revision"].as_u64().unwrap();

        let png = one_pixel();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let body = json!({"png": b64, "x": 1, "y": 2, "width": 1, "height": 1}).to_string();
        let posted = format!(
            "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        {
            let urls = opened.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(urls.len(), 1, "{urls:?}");
            assert!(urls[0].ends_with(&format!(":{}/en", addr.port())), "{}", urls[0]);
        }

        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        assert_eq!(opened.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);

        let (status, _, after) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        let after: Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(after["state"], "waiting");
        assert_eq!(after["revision"].as_u64().unwrap(), revision + 2);
    }

    #[test]
    fn use_this_keeps_that_image_and_a_second_press_does_not_count_again() {
        let _hold = hold_state();
        {
            let mut g = state();
            g.selection = None;
            g.result = None;
            g.result_revision = 0;
            g.accepted = None;
            g.accepted_result = 0;
            g.accept_seq = 0;
            g.placed = 0;
            g.generating = false;
            g.error = None;
            g.revision = 0;
        }
        let png = one_pixel();
        let fake = Arc::new(Fake { png: png.clone(), seen: Mutex::new(None) });
        let keys = Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-confirm"), Some("present".into())));
        let server = start("127.0.0.1:0".parse().unwrap(), fake, keys, Lang::En, None).unwrap();
        let addr = server.addr;

        let (status, _, _) =
            exchange(addr, "POST /confirm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 400);

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let body = json!({"png": b64, "x": 8, "y": 9, "width": 1, "height": 1}).to_string();
        let posted = format!(
            "POST /selection HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");

        let prompt_body = r#"{"prompt":"remove stray hair"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");

        {
            let mut g = state();
            g.generating = true;
        }
        let (status, _, _) =
            exchange(addr, "POST /confirm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 409);
        {
            let mut g = state();
            g.generating = false;
        }

        let (status, head, resp) =
            exchange(addr, "POST /confirm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 200, "{head}");
        let confirmed: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(confirmed["accept"], 1);
        assert_eq!(confirmed["resultRevision"], 1);

        let (status, head, again) =
            exchange(addr, "POST /confirm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 200, "{head}");
        let again: Value = serde_json::from_slice(&again).unwrap();
        assert_eq!(again["accept"], 1, "the same image is not a second confirmation");

        let (status, head, body) = exchange(addr, "GET /accepted HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{head}");
        assert_eq!(body, png);
        assert!(head.to_ascii_lowercase().contains("x-origin-x: 8"));

        let (status, _, status_body) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        let status_json: Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_json["accept"], 1);
        assert_eq!(status_json["placed"], 0);
        assert_eq!(status_json["acceptedResult"], 1);

        let (status, _, placed_body) =
            exchange(addr, "POST /placed HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 200);
        let placed: Value = serde_json::from_slice(&placed_body).unwrap();
        assert_eq!(placed["placed"], 1);

        let prompt_body = r#"{"prompt":"retouch skin"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, _) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        let (status, _, kept) = exchange(addr, "GET /accepted HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(kept, png, "regenerate does not replace the confirmed image");

        let (status, _, status_body) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200);
        let status_json: Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_json["resultRevision"], 2);
        assert_eq!(status_json["acceptedResult"], 1, "the new image is not confirmed yet");

        let (status, _, resp) =
            exchange(addr, "POST /confirm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(status, 200);
        let confirmed: Value = serde_json::from_slice(&resp).unwrap();
        assert_eq!(confirmed["accept"], 2);
        assert_eq!(confirmed["resultRevision"], 2);
    }
}
