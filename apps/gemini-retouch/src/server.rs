//! The local HTTP server. PhotoCraft uploads a square PNG; the browser page on `/` is where
//! the prompt is typed and sent. The finished image is fetched back by PhotoCraft.

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

#[derive(Clone)]
struct Selection {
    png: Vec<u8>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

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
    generating: bool,
    error: Option<String>,
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
        json!({
            "state": state,
            "error": self.error,
            "width": width,
            "height": height,
            "x": x,
            "y": y,
            "imageSize": self.selection.as_ref().map(|s| gemini::image_size_for(s.width)),
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
pub fn start(addr: SocketAddr, gemini: Arc<dyn Gemini>, keys: Arc<KeyStore>, lang: Lang) -> Result<Server, String> {
    let listener = TcpListener::bind(addr).map_err(|e| lang.err_listen(&addr, &e))?;
    listener.set_nonblocking(false).map_err(|e| lang.err_listen(&addr, &e))?;
    let bound = listener.local_addr().map_err(|e| lang.err_listen(&addr, &e))?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let thread = std::thread::Builder::new()
        .name("retouch-http".into())
        .spawn(move || accept_loop(listener, gemini, keys, flag, lang))
        .map_err(|e| lang.err_thread(&e))?;
    Ok(Server { addr: bound, shutdown, thread: Some(thread) })
}

fn accept_loop(listener: TcpListener, gemini: Arc<dyn Gemini>, keys: Arc<KeyStore>, shutdown: Arc<AtomicBool>, lang: Lang) {
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
        let _ = std::thread::Builder::new().name("retouch-req".into()).spawn(move || {
            if let Err(e) = handle(sock, gemini.as_ref(), keys.as_ref()) {
                let kind = e.kind();
                if kind != std::io::ErrorKind::UnexpectedEof && kind != std::io::ErrorKind::ConnectionAborted && kind != std::io::ErrorKind::ConnectionReset {
                    eprintln!("{}", req_lang.log_backend(&e));
                }
            }
        });
    }
}

fn handle(mut sock: TcpStream, gemini: &dyn Gemini, keys: &KeyStore) -> std::io::Result<()> {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(30)));
    let req = match read_request(&mut sock) {
        Ok(r) => r,
        Err(e) => {
            let _ = write_response(&mut sock, 400, "text/plain; charset=utf-8", &[], e.to_string().as_bytes());
            return Ok(());
        }
    };
    let response = route(&req, gemini, keys);
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

fn route(req: &Incoming, gemini: &dyn Gemini, keys: &KeyStore) -> Outgoing {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => html(PAGE),
        ("GET", "/key-status") => json_out(200, &json!({"set": keys.has_key()})),
        ("POST", "/api-key") => save_key(req, keys),
        ("GET", "/status") => json_out(200, &state().status()),
        ("GET", "/selection.png") => selection_png(),
        ("GET", "/result") => result_png(),
        ("POST", "/selection") => store_selection(&req.body),
        ("POST", "/generate") => generate(req, gemini),
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
    STATE.get_or_init(|| Mutex::new(State { selection: None, result: None, generating: false, error: None }))
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

fn store_selection(body: &[u8]) -> Outgoing {
    let parsed = match parse_selection(body) {
        Ok(s) => s,
        Err(e) => return text(400, &e),
    };
    let image_size = gemini::image_size_for(parsed.width);
    let width = parsed.width;
    let height = parsed.height;
    let mut g = state();
    g.error = None;
    g.result = None;
    g.selection = Some(parsed);
    json_out(200, &json!({"ok": true, "width": width, "height": height, "imageSize": image_size}))
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
    gemini::check_square(width, height)?;
    let x = signed(&v, "x")?;
    let y = signed(&v, "y")?;
    Ok(Selection { png, x, y, width, height })
}

fn generate(req: &Incoming, gemini: &dyn Gemini) -> Outgoing {
    let prompt = match prompt_from(req) {
        Ok(p) => p,
        Err(e) => return text(400, &e),
    };
    let selection = {
        let mut g = state();
        if g.generating {
            return text(409, "a retouch is already running");
        }
        let Some(sel) = g.selection.clone() else {
            return text(400, "no selection yet; send a square selection from PhotoCraft first");
        };
        g.generating = true;
        g.error = None;
        sel
    };
    finish_edit(&selection.png, selection.x, selection.y, selection.width, &prompt, gemini)
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
    {
        let mut g = state();
        if g.generating {
            return text(409, "a retouch is already running");
        }
        g.generating = true;
        g.error = None;
        g.selection = Some(Selection { png: selection.png.clone(), x: selection.x, y: selection.y, width: selection.width, height: selection.height });
    }
    finish_edit(&selection.png, selection.x, selection.y, selection.width, &prompt, gemini)
}

fn finish_edit(png: &[u8], x: i32, y: i32, side: u32, prompt: &str, gemini: &dyn Gemini) -> Outgoing {
    let image_size = gemini::image_size_for(side).to_string();
    let edited = gemini.edit(png, side, prompt);
    let mut g = state();
    g.generating = false;
    match edited {
        Ok(out) => {
            let finished = Finished { png: out, x, y, width: side, height: side, image_size };
            let response = png_out(&finished.png, &finished);
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
    let mut head = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
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

const PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>PhotoCraft retouch</title>
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
  input { flex: 1; min-width: 12em; font: inherit; padding: 8px 10px; background: #111; color: inherit; border: 1px solid #444; }
  #keybox { margin: 0 0 18px; padding: 12px 14px; border: 1px solid #444; }
  a { color: #9eb6ff; }
  #status { margin-top: 14px; min-height: 1.4em; }
  .err { color: #ff8a80; }
  .ok { color: #9ccc9c; }
</style>
</head>
<body>
<main>
  <h1>PhotoCraft retouch</h1>
  <p>Select a square in PhotoCraft and choose Edit › Generative Retouch. Then describe the change and send it. When this page says the image is ready, choose Edit › Place Generative Result.</p>
  <div id="keybox" hidden>
    <p>Paste the API key from <a href="https://aistudio.google.com/apikey">Google AI Studio</a>. It stays on this computer. PhotoCraft never sees it.</p>
    <div class="row">
      <input id="apikey" type="password" autocomplete="off" spellcheck="false" placeholder="API key">
      <button type="button" class="primary" id="savekey">Save key</button>
    </div>
  </div>
  <div id="preview" class="empty">No selection yet.</div>
  <textarea id="prompt" placeholder="remove stray hair"></textarea>
  <div class="row">
    <button type="button" id="hair">Remove stray hair</button>
    <button type="button" id="skin">Retouch skin</button>
    <button type="button" class="primary" id="send">Send</button>
  </div>
  <div id="status"></div>
</main>
<script>
const prompt = document.getElementById("prompt");
const status = document.getElementById("status");
const preview = document.getElementById("preview");
const send = document.getElementById("send");
const keybox = document.getElementById("keybox");
const apikey = document.getElementById("apikey");
let keySet = false;
document.getElementById("hair").onclick = () => { prompt.value = "remove stray hair"; };
document.getElementById("skin").onclick = () => { prompt.value = "retouch skin"; };
document.getElementById("savekey").onclick = async () => {
  note("Saving the key…");
  try {
    const res = await fetch("/api-key", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key: apikey.value }) });
    const msg = await res.json();
    if (!res.ok) { note(msg.error || "Could not save the key.", "err"); return; }
    apikey.value = "";
    note("API key saved on this computer.", "ok");
    refresh();
  } catch (e) {
    note("Could not reach the backend.", "err");
  }
};
function note(text, cls) { status.textContent = text; status.className = cls || ""; }
async function refresh() {
  let info;
  try { info = await (await fetch("/status")).json(); }
  catch (e) { note("Could not reach the backend.", "err"); return; }
  try {
    const keyInfo = await (await fetch("/key-status")).json();
    keySet = !!keyInfo.set;
    keybox.hidden = keySet;
  } catch (e) { /* the prompt page still works; Send will say if the key is missing */ }
  if (info.state === "waiting" || info.state === "ready" || info.state === "generating" || info.state === "error") {
    if (!preview.querySelector("img")) {
      const img = document.createElement("img");
      img.alt = "Selected square";
      img.src = "/selection.png?t=" + Date.now();
      preview.className = "";
      preview.textContent = "";
      preview.appendChild(img);
    }
  }
  if (info.state === "generating") note("Sending to Gemini…");
  else if (info.state === "ready") note("Ready. In PhotoCraft, choose Edit › Place Generative Result.", "ok");
  else if (info.state === "error") note(info.error || "The retouch failed.", "err");
  else if (info.state === "waiting") note("Selection received (" + info.width + "×" + info.height + ", output " + info.imageSize + "). Enter a prompt and send.");
  else if (!keySet) note("Paste your Gemini API key above. It is saved on this computer.");
  else if (status.className !== "ok") note("");
  send.disabled = !keySet || info.state === "generating" || info.state === "empty";
}
send.onclick = async () => {
  send.disabled = true;
  note("Sending to Gemini…");
  try {
    const res = await fetch("/generate", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ prompt: prompt.value }) });
    const kind = res.headers.get("content-type") || "";
    if (!res.ok) {
      const msg = kind.includes("json") ? (await res.json()).error : await res.text();
      note(msg || "The retouch failed.", "err");
    } else {
      note("Ready. In PhotoCraft, choose Edit › Place Generative Result.", "ok");
    }
  } catch (e) {
    note("Could not reach the backend.", "err");
  }
  refresh();
};
refresh();
setInterval(refresh, 2000);
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

    struct Fake {
        png: Vec<u8>,
        seen: Mutex<Option<(u32, String)>>,
    }

    impl Gemini for Fake {
        fn edit(&self, _png: &[u8], side: u32, prompt: &str) -> Result<Vec<u8>, String> {
            *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some((side, prompt.to_string()));
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
        // The process-global state is shared, so this test owns it for the duration.
        {
            let mut g = state();
            g.selection = None;
            g.result = None;
            g.generating = false;
            g.error = None;
        }
        let png = one_pixel();
        let fake = Arc::new(Fake { png: png.clone(), seen: Mutex::new(None) });
        let keys = Arc::new(crate::key::KeyStore::open(std::env::temp_dir().join("photocraft-retouch-unused-key"), Some("present".into())));
        let server = start("127.0.0.1:0".parse().unwrap(), fake.clone(), keys, Lang::En).unwrap();
        let addr = server.addr;

        let (status, page, _) = exchange(addr, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{page}");
        assert!(page.contains("PhotoCraft retouch"));

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
        assert_eq!(seen, (1, "remove stray hair".into()));

        // A finished result must not disable Send. The page only disables it with no key,
        // while a retouch is running, or before any selection has arrived.
        let (status, head, status_body) = exchange(addr, "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(status, 200, "{head}");
        let status_json: Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_json["state"], "ready");
        assert!(PAGE.contains(r#"send.disabled = !keySet || info.state === "generating" || info.state === "empty""#));

        let prompt_body = r#"{"prompt":"retouch skin"}"#;
        let posted = format!(
            "POST /generate HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let (status, head, body) = exchange(addr, &posted);
        assert_eq!(status, 200, "{head}");
        assert_eq!(body, png);
        let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
        assert_eq!(seen, (1, "retouch skin".into()));

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

    #[test]
    fn the_page_saves_an_api_key_without_repeating_it() {
        let dir = std::env::temp_dir().join(format!("photocraft-retouch-http-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gemini-api-key.txt");
        let keys = Arc::new(crate::key::KeyStore::open(path.clone(), Some(String::new())));
        let fake = Arc::new(Fake { png: Vec::new(), seen: Mutex::new(None) });
        let server = start("127.0.0.1:0".parse().unwrap(), fake, Arc::clone(&keys), Lang::En).unwrap();

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
}
