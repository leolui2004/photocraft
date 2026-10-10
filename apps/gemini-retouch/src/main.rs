//! Process entry for the retouch backend. The library owns the server.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() {
    if let Err(e) = photocraft_gemini_retouch::run() {
        eprintln!("photocraft-gemini-retouch: {e}");
        std::process::exit(1);
    }
}
