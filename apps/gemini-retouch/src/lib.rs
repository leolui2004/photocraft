//! Local retouch backend for PhotoCraft.
//!
//! PhotoCraft sends a selection here. The prompt and aspect ratio are entered on the page this
//! server hosts (or passed through by an agent), and the server calls Gemini Nano Banana 2.1.
//! PhotoCraft then fetches the image and places it on a new layer. The API key stays here and
//! never enters the editor. A shipped exe (release build, installer, portable zip) only uses the
//! key the user pasted into the page. Debug builds (`cargo run`) may also read `GEMINI_API_KEY`
//! from the environment or a `.env` file.

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
/// Logs and the prompt page are English unless `--lang ja` or `--lang zh-Hant` is passed
/// (`--help` lists them). The browser opens on that page unless `--no-browser` is passed.
/// PhotoCraft passes `--no-browser` and opens one page itself after the selection is stored.
/// `--open-on-selection` delays this process's own browser until the first selection arrives.
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
    // The hook is installed before the accept loop runs. PhotoCraft can upload as soon as
    // the port answers, which is still after this closure exists.
    let on_first_selection =
        if opts.open_browser && opts.open_on_selection { Some(Arc::new(|url: String| open_browser(&url)) as server::FirstSelectionHook) } else { None };
    let server = server::start(cfg.addr, gemini, Arc::clone(&cfg.keys), opts.lang, on_first_selection)?;
    let page = http_origin(server.addr);
    eprintln!("{}", opts.lang.banner_listening(&server.addr));
    eprintln!("{}", opts.lang.banner_howto());
    eprintln!("{}", opts.lang.banner_leave_open());
    if !cfg.keys.has_key() {
        eprintln!("{}", opts.lang.err_missing_key());
    }
    if opts.open_browser && !opts.open_on_selection {
        open_browser(&format!("{page}{}", opts.lang.page_code()));
    }
    loop {
        std::thread::park();
    }
}

pub(crate) fn http_origin(addr: SocketAddr) -> String {
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
    // Release builds are what installers and the portable zip ship. They must not open `.env`
    // or honor `GEMINI_API_KEY`, even when the exe sits inside a developer checkout.
    let shipped = !cfg!(debug_assertions);
    let file = dotenv_for_launch(shipped);
    let preset = developer_api_key(shipped, std::env::var("GEMINI_API_KEY").ok(), &file);
    let keys = Arc::new(KeyStore::load(preset));
    let addr_s = env_or(&file, "RETOUCH_ADDR").unwrap_or_else(|| "127.0.0.1:8787".into());
    let addr: SocketAddr = addr_s.parse().map_err(|_| lang.err_bad_addr(&addr_s))?;
    if !addr.ip().is_loopback() {
        return Err(lang.err_not_loopback().to_string());
    }
    let gemini_url = env_or(&file, "GEMINI_API_URL").unwrap_or_else(|| gemini::DEFAULT_URL.to_string());
    Ok(Config { keys, addr, gemini_url })
}

/// `.env` is a developer convenience for `cargo run`. A shipped exe gets an empty map and does
/// not walk parent directories, so a portable copy inside this repo cannot see the checkout's file.
fn dotenv_for_launch(shipped_exe: bool) -> HashMap<String, String> {
    if shipped_exe { HashMap::new() } else { read_dotenv() }
}

/// `None` on a shipped exe: the page asks, and the saved paste is the only key. Debug builds
/// take the process environment first, then `.env`.
fn developer_api_key(shipped_exe: bool, process_key: Option<String>, file: &HashMap<String, String>) -> Option<String> {
    if shipped_exe {
        return None;
    }
    match process_key {
        Some(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => file.get("GEMINI_API_KEY").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    }
}

/// `.env` in the working directory or a parent (so `cargo run -p` from the repo root and from
/// this crate both find it). Only called for debug builds.
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
    fn a_shipped_exe_ignores_dotenv_and_the_environment() {
        let mut file = HashMap::new();
        file.insert("GEMINI_API_KEY".into(), "from-file".into());
        file.insert("RETOUCH_ADDR".into(), "127.0.0.1:9".into());
        assert_eq!(developer_api_key(true, Some("from-env".into()), &file), None);
        assert!(dotenv_for_launch(true).is_empty());
        assert_eq!(developer_api_key(false, Some("  from-env  ".into()), &file).as_deref(), Some("from-env"));
        assert_eq!(developer_api_key(false, Some(String::new()), &file).as_deref(), Some("from-file"));
        assert_eq!(developer_api_key(false, None, &file).as_deref(), Some("from-file"));
        assert_eq!(env_or(&HashMap::new(), "RETOUCH_ADDR"), None);
    }

    #[test]
    fn env_file_parses_and_skips_comments() {
        let map = parse_env("# comment\nexport GEMINI_API_KEY=\"abc\"\nRETOUCH_ADDR=127.0.0.1:9\n\nnot a pair\n");
        assert_eq!(map.get("GEMINI_API_KEY").map(String::as_str), Some("abc"));
        assert_eq!(map.get("RETOUCH_ADDR").map(String::as_str), Some("127.0.0.1:9"));
        assert!(!map.contains_key("not"));
    }
}
