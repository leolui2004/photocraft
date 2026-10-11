//! Start the local retouch helper when Edit › Generative Retouch needs it.
//!
//! The helper is a separate program (`photocraft-gemini-retouch`) installed next to PhotoCraft.
//! It holds the Gemini API key. This module only checks that it is listening, and starts it
//! when it is not.

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
        if port_open(ADDR) {
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

fn spawn(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(path);
    // PhotoCraft opens the page itself, after the selection has been uploaded.
    cmd.arg("--no-browser");
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
}
