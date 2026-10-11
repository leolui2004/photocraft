//! Local retouch backend for PhotoCraft.
//!
//! PhotoCraft sends a square selection here. The prompt is entered on the page this server
//! hosts (or passed through by an agent), and the server calls Gemini Nano Banana 2.1.
//! PhotoCraft then fetches the image and places it on a new layer. The API key stays here
//! (environment, `.env`, or the key saved from the browser page) and never enters the editor.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod gemini;
mod key;
mod lang;
mod server;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

pub use gemini::{Gemini, UreqGemini};
pub use key::KeyStore;
pub use server::Server;

/// Bind and serve until the process is stopped.
///
/// Logs are English unless `--lang ja` or `--lang zh-Hant` is passed (`--help` lists them).
/// The browser opens on the prompt page unless `--no-browser` is passed (PhotoCraft passes that,
/// because it opens the page itself after uploading the selection).
pub fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run_args(&args)
}

fn run_args(args: &[impl AsRef<str>]) -> Result<(), String> {
    let opts = lang::parse_args(args)?;
    if opts.help {
        println!("{}", opts.lang.help());
        return Ok(());
    }
    let cfg = load_config(opts.lang)?;
    let gemini = Arc::new(UreqGemini::new(Arc::clone(&cfg.keys), cfg.gemini_url, opts.lang));
    let server = server::start(cfg.addr, gemini, Arc::clone(&cfg.keys), opts.lang)?;
    let page = http_origin(server.addr);
    eprintln!("{}", opts.lang.banner_listening(&server.addr));
    eprintln!("{}", opts.lang.banner_howto());
    eprintln!("{}", opts.lang.banner_leave_open());
    if !cfg.keys.has_key() {
        eprintln!("{}", opts.lang.err_missing_key());
    }
    if opts.open_browser {
        open_browser(&page);
    }
    loop {
        std::thread::park();
    }
}

fn http_origin(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V4(ip) => format!("http://{ip}:{}/", addr.port()),
        IpAddr::V6(ip) => format!("http://[{ip}]:{}/", addr.port()),
    }
}

/// Best-effort. A missing browser still leaves the address on the console.
fn open_browser(url: &str) {
    let spawned = if cfg!(target_os = "windows") {
        // `start` treats the first quoted argument as a window title.
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        eprintln!("could not open a browser ({e}). Open {url} yourself.");
    }
}

struct Config {
    keys: Arc<KeyStore>,
    addr: SocketAddr,
    gemini_url: String,
}

fn load_config(lang: lang::Lang) -> Result<Config, String> {
    let file = read_dotenv();
    let preset = env_or(&file, "GEMINI_API_KEY").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let keys = Arc::new(KeyStore::load(preset));
    let addr_s = env_or(&file, "RETOUCH_ADDR").unwrap_or_else(|| "127.0.0.1:8787".into());
    let addr: SocketAddr = addr_s.parse().map_err(|_| lang.err_bad_addr(&addr_s))?;
    if !addr.ip().is_loopback() {
        return Err(lang.err_not_loopback().to_string());
    }
    let gemini_url = env_or(&file, "GEMINI_API_URL").unwrap_or_else(|| gemini::DEFAULT_URL.to_string());
    Ok(Config { keys, addr, gemini_url })
}

/// `.env` in the working directory or a parent (so `cargo run -p` from the repo root and from
/// this crate both find it). Process environment wins over the file.
fn read_dotenv() -> HashMap<String, String> {
    let Ok(mut dir) = std::env::current_dir() else { return HashMap::new() };
    for _ in 0..8 {
        let path = dir.join(".env");
        if path.is_file() {
            return parse_env_file(&path);
        }
        if !dir.pop() {
            break;
        }
    }
    HashMap::new()
}

fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashMap::new() };
    parse_env(&text)
}

fn parse_env(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || key.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '_')) {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        out.insert(key.to_string(), value.to_string());
    }
    out
}

fn env_or(file: &HashMap<String, String>, key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => file.get(key).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_parses_and_skips_comments() {
        let map = parse_env("# comment\nexport GEMINI_API_KEY=\"abc\"\nRETOUCH_ADDR=127.0.0.1:9\n\nnot a pair\n");
        assert_eq!(map.get("GEMINI_API_KEY").map(String::as_str), Some("abc"));
        assert_eq!(map.get("RETOUCH_ADDR").map(String::as_str), Some("127.0.0.1:9"));
        assert!(!map.contains_key("not"));
    }
}
