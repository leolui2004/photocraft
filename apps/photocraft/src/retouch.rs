//! Start the local retouch helper when Edit › Generative Retouch needs it.
//!
//! The helper is a separate program (`photocraft-gemini-retouch`) installed next to PhotoCraft.
//! It holds the Gemini API key. This module only checks that it is listening, and starts it
//! when it is not. The helper is started with `--no-browser`: PhotoCraft opens exactly one
//! prompt page after the selection upload returns. Opening from the helper as well, or with
//! `cmd /c start` while the new console is still attaching, shows only the console on the
//! first click and two copies of the page on the next.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const ADDR: &str = "127.0.0.1:8787";

/// The file name of the helper next to the PhotoCraft executable.
pub fn helper_name() -> &'static str {
    if cfg!(windows) { "photocraft-gemini-retouch.exe" } else { "photocraft-gemini-retouch" }
}

/// Make sure the helper is listening. Starts the copy installed beside this app.
/// The caller opens the prompt page after the selection has been stored.
pub fn ensure() -> Result<(), String> {
    if port_open(ADDR) {
        return Ok(());
    }
    let path = installed_helper().ok_or_else(|| {
        "PhotoCraft Retouch is not installed next to PhotoCraft. On Windows, open PhotoCraft Retouch from the Start menu and leave its window open.".to_string()
    })?;
    spawn(&path)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if http_ready(ADDR) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("PhotoCraft Retouch did not start. Open it from the Start menu and leave its window open, then try again.".into())
}

fn installed_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    helper_in(dir)
}

fn helper_in(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(helper_name());
    path.is_file().then_some(path)
}

/// Arguments passed to a helper this app starts. `--no-browser` so only PhotoCraft opens the
/// page, after the selection is stored, in the same language as the PhotoCraft window.
fn launch_args(lang: &str) -> Vec<String> {
    vec!["--lang".into(), lang.into(), "--no-browser".into()]
}

fn spawn(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(path);
    cmd.args(launch_args(photocraft_ui_egui::i18n::retouch_page_code()));
    if let Some(dir) = path.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // A new console window, so the helper stays up after PhotoCraft closes and the user
        // can see "leave this window open".
        cmd.creation_flags(0x0000_0010);
    }
    let child = cmd.spawn().map_err(|e| format!("could not start PhotoCraft Retouch: {e}"))?;
    // Reap the helper when it exits so it does not stay a zombie. The thread blocks until then.
    let _ = std::thread::Builder::new().name("retouch-helper".into()).spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

fn port_open(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = addr.to_socket_addrs() else { return false };
    let Some(addr) = addrs.next() else { return false };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// The port accepts a connection and answers the status request. A bare connect can succeed
/// before the helper is ready to take the selection upload.
fn http_ready(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = addr.to_socket_addrs() else { return false };
    let Some(sock_addr) = addrs.next() else { return false };
    let Ok(mut stream) = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(200)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let req = format!("GET /key-status HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 4096 {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    head.contains("200") && head.contains("\"set\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn the_helper_is_the_program_beside_photocraft() {
        let dir = std::env::temp_dir().join(format!("photocraft-retouch-helper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(helper_in(&dir).is_none());
        let path = dir.join(helper_name());
        std::fs::write(&path, b"").unwrap();
        assert_eq!(helper_in(&dir).as_deref(), Some(path.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_closed_port_is_not_open_and_a_listener_is() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(port_open(&addr.to_string()));
        drop(listener);
        assert!(!port_open(&addr.to_string()));
    }

    #[test]
    fn the_helper_starts_without_opening_a_browser() {
        let args = launch_args("ja");
        assert_eq!(args, ["--lang", "ja", "--no-browser"]);
        assert!(!args.iter().any(|a| a == "--open-on-selection"));
    }

    #[test]
    fn a_closed_port_is_not_ready_for_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!http_ready(&addr.to_string()));
    }
}
