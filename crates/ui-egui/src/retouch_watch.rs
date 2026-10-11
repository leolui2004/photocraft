//! After Edit › Generative Retouch opens the prompt page, watch for Use this and place that image.
//!
//! The page confirms a result on the local backend. This polls that counter off the UI thread and
//! places the confirmed image once. Regenerate on the page does not place anything. Confirms that
//! already happened before this watch started are left alone.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

use crate::PhotocraftApp;

/// The baseline status read failed. The next successful read is the baseline, not a new confirm.
const UNKNOWN: u64 = u64::MAX;

pub struct Watch {
    origin: String,
    seen: u64,
    fails: u8,
    latest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Watch {
    fn spawn(origin: String, seen: u64, ctx: egui::Context) -> Self {
        let latest = Arc::new(AtomicU64::new(seen));
        let stop = Arc::new(AtomicBool::new(false));
        let watch = Self { origin: origin.clone(), seen, fails: 0, latest: Arc::clone(&latest), stop: Arc::clone(&stop) };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = std::thread::Builder::new().name("retouch-watch".into()).spawn(move || poll_loop(origin, latest, stop, ctx));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (origin, latest, stop, ctx);
        }
        watch
    }
}

/// Remember that `page` is where the prompt is typed. The confirm counter at this moment is the
/// baseline: only a later Use this places a layer.
pub fn arm(app: &mut PhotocraftApp, page: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, page);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let origin = origin_from_page(page);
        if app.retouch_watch.as_ref().is_some_and(|w| w.origin == origin) {
            return;
        }
        if let Some(ctx) = app.frame_ctx.clone() {
            app.retouch_arm = None;
            let seen = photocraft_engine::gemini_cmds::retouch_accept_seq(&origin).unwrap_or(UNKNOWN);
            app.retouch_watch = Some(Watch::spawn(origin, seen, ctx));
        } else {
            app.retouch_arm = Some(origin);
        }
    }
}

pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, ctx);
    }
    #[cfg(not(target_arch = "wasm32"))]
    tick_native(app, ctx);
}

#[cfg(not(target_arch = "wasm32"))]
fn tick_native(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if let Some(origin) = app.retouch_arm.take() {
        let seen = photocraft_engine::gemini_cmds::retouch_accept_seq(&origin).unwrap_or(UNKNOWN);
        app.retouch_watch = Some(Watch::spawn(origin, seen, ctx.clone()));
    }
    let Some(latest) = app.retouch_watch.as_ref().map(|w| w.latest.load(Ordering::Relaxed)) else {
        return;
    };
    let seen = match app.retouch_watch.as_ref() {
        Some(w) => w.seen,
        None => return,
    };
    if seen == UNKNOWN {
        if latest != UNKNOWN
            && let Some(w) = app.retouch_watch.as_mut()
        {
            w.seen = latest;
        }
        return;
    }
    if latest == UNKNOWN || latest <= seen {
        return;
    }
    let fails = app.retouch_watch.as_ref().map(|w| w.fails).unwrap_or(0);
    if fails >= 3 {
        return;
    }
    if app.session.active().is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(400));
        return;
    }
    let origin = match app.retouch_watch.as_ref() {
        Some(w) => w.origin.clone(),
        None => return,
    };
    match app.run("edit.generativeRetouch.place", json!({"url": origin, "accepted": true})) {
        Ok(_) => {
            if let Some(w) = app.retouch_watch.as_mut() {
                w.seen = latest;
                w.fails = 0;
            }
            app.ui.status = tl!("Added the generative result.").to_string();
            app.ui.status_error = false;
        }
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
            if let Some(w) = app.retouch_watch.as_mut() {
                w.fails = w.fails.saturating_add(1);
                if w.fails >= 3 {
                    w.seen = latest;
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(400));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_loop(origin: String, latest: Arc<AtomicU64>, stop: Arc<AtomicBool>, ctx: egui::Context) {
    let mut prev = latest.load(Ordering::Relaxed);
    while !stop.load(Ordering::Relaxed) {
        if let Ok(n) = photocraft_engine::gemini_cmds::retouch_accept_seq(&origin)
            && (prev == UNKNOWN || n != prev)
        {
            prev = n;
            latest.store(n, Ordering::Relaxed);
            ctx.request_repaint();
        }
        for _ in 0..8 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// `http://127.0.0.1:8787/ja` → `http://127.0.0.1:8787`.
fn origin_from_page(page: &str) -> String {
    let Some(rest) = page.strip_prefix("http://") else {
        return page.to_string();
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    if hostport.is_empty() {
        return page.to_string();
    }
    format!("http://{hostport}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_url_drops_its_language_path() {
        assert_eq!(origin_from_page("http://127.0.0.1:8787/ja"), "http://127.0.0.1:8787");
        assert_eq!(origin_from_page("http://127.0.0.1:8787/zh-Hant"), "http://127.0.0.1:8787");
        assert_eq!(origin_from_page("http://127.0.0.1:8787/"), "http://127.0.0.1:8787");
        assert_eq!(origin_from_page("http://127.0.0.1:8787"), "http://127.0.0.1:8787");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod place_tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::PhotocraftApp;
    use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
    use serde_json::json;

    #[allow(clippy::unwrap_used)] // compound `cfg(test)` is not clippy's test-module allow
    fn one_pixel() -> Vec<u8> {
        let img = Image::from_u8(1, 1, ChannelLayout::Rgba, vec![0, 180, 0, 255]).unwrap();
        photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap()
    }

    #[test]
    fn use_this_on_the_page_places_a_layer() {
        let accept = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_w = Arc::clone(&accept);
        let stop_w = Arc::clone(&stop);
        let png = one_pixel();
        std::thread::spawn(move || {
            while !stop_w.load(Ordering::Relaxed) {
                let Ok((mut sock, _)) = listener.accept() else { break };
                if stop_w.load(Ordering::Relaxed) {
                    break;
                }
                let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                loop {
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    match sock.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf);
                let path = head.split(' ').nth(1).unwrap_or("");
                let seq = accept_w.load(Ordering::Relaxed);
                let body: Vec<u8> = if path.starts_with("/accepted") {
                    png.clone()
                } else if path.starts_with("/placed") {
                    br#"{"ok":true}"#.to_vec()
                } else {
                    format!(r#"{{"accept":{seq}}}"#).into_bytes()
                };
                let kind = if path.starts_with("/accepted") { "image/png" } else { "application/json" };
                let extra = if path.starts_with("/accepted") {
                    "X-Origin-X: 2\r\nX-Origin-Y: 3\r\nX-Origin-Width: 1\r\nX-Origin-Height: 1\r\nX-Image-Size: 1K\r\n"
                } else {
                    ""
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.write_all(&body);
            }
        });

        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        let page = format!("http://{addr}/en");
        let ctx = egui::Context::default();
        arm(&mut app, &page);
        tick(&mut app, &ctx);
        assert!(app.retouch_watch.as_ref().is_some_and(|w| w.seen == 0), "confirms that already happened are not placed");
        assert_eq!(app.session.active().map(|d| d.doc.layers.len()), Some(1));

        accept.store(1, Ordering::Relaxed);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            tick(&mut app, &ctx);
            if app.session.active().is_some_and(|d| d.doc.layers.len() == 2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers.len(), 2, "Use this adds the confirmed image ({})", app.ui.status);
        assert!(doc.layers.iter().any(|l| l.name.starts_with("Generative Retouch")));
        stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(addr);
    }
}
