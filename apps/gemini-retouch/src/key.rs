//! The Gemini API key. It stays in this process: an environment variable, a `.env` file, or a
//! file written when the user pastes the key into the browser page. PhotoCraft never sees it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

pub struct KeyStore {
    key: Mutex<String>,
    path: PathBuf,
}

impl KeyStore {
    /// `preset` is the key from the environment or `.env` (it wins over the saved file).
    pub fn load(preset: Option<String>) -> Self {
        let path = key_path();
        let key = match preset.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(k) => k,
            None => read_stored(&path).unwrap_or_default(),
        };
        Self { key: Mutex::new(key), path }
    }

    /// `preset` skips reading `path` (`Some("")` starts with no key).
    pub fn open(path: PathBuf, preset: Option<String>) -> Self {
        let key = match preset {
            Some(k) => k,
            None => read_stored(&path).unwrap_or_default(),
        };
        Self { key: Mutex::new(key), path }
    }

    pub fn has_key(&self) -> bool {
        !self.get().is_empty()
    }

    pub fn get(&self) -> String {
        self.key.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Check the pasted key and save it where the next launch will find it.
    pub fn set(&self, raw: &str) -> Result<(), String> {
        let key = validate_key(raw)?;
        write_private(&self.path, &key)?;
        *self.key.lock().unwrap_or_else(PoisonError::into_inner) = key;
        Ok(())
    }
}

/// A single pasted token: no spaces, no quotes, not a paragraph.
pub fn validate_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("paste the API key from Google AI Studio".into());
    }
    if key.chars().count() > 256 {
        return Err("that API key is too long".into());
    }
    if !key.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\'') {
        return Err("the API key should be the single line copied from Google AI Studio, with no spaces".into());
    }
    Ok(key.to_string())
}

fn key_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RETOUCH_KEY_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let list = candidate_paths();
    if let Some(found) = list.iter().find(|p| p.is_file()) {
        return found.clone();
    }
    for candidate in &list {
        let Some(dir) = candidate.parent() else { continue };
        if std::fs::create_dir_all(dir).is_ok() {
            return candidate.clone();
        }
    }
    list.into_iter().next().unwrap_or_else(|| PathBuf::from("gemini-api-key.txt"))
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && (dir.join("portable.txt").is_file() || dir.join("PhotoCraft.portable").is_file())
    {
        out.push(dir.join("PhotoCraftData").join("gemini-api-key.txt"));
    }
    if let Some(dir) = platform_config_dir() {
        out.push(dir.join("gemini-api-key.txt"));
    }
    out
}

#[cfg(windows)]
fn platform_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(|p| PathBuf::from(p).join("Photocraft"))
}

#[cfg(target_os = "macos")]
fn platform_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|p| !p.is_empty()).map(|p| PathBuf::from(p).join("Library/Application Support/Photocraft"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_config_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p).join("photocraft"));
    }
    std::env::var_os("HOME").filter(|p| !p.is_empty()).map(|p| PathBuf::from(p).join(".config/photocraft"))
}

#[cfg(not(any(windows, unix)))]
fn platform_config_dir() -> Option<PathBuf> {
    None
}

fn read_stored(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().map(str::trim).find(|s| !s.is_empty())?;
    validate_key(line).ok()
}

fn write_private(path: &Path, key: &str) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| format!("could not save the API key to {}: {e}", path.display()))?;
    file.write_all(key.as_bytes()).map_err(|e| format!("could not save the API key to {}: {e}", path.display()))?;
    file.write_all(b"\n").map_err(|e| format!("could not save the API key to {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_key_is_one_line_and_roundtrips() {
        assert!(validate_key("").is_err());
        assert!(validate_key("   ").is_err());
        assert!(validate_key("has a space").is_err());
        assert!(validate_key("quote\"inside").is_err());
        assert!(validate_key(&"x".repeat(257)).is_err());
        assert_eq!(validate_key("  AIzaSyExampleKey123  ").unwrap(), "AIzaSyExampleKey123");

        let dir = std::env::temp_dir().join(format!("photocraft-retouch-key-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gemini-api-key.txt");
        let store = KeyStore::open(path.clone(), Some(String::new()));
        assert!(!store.has_key());
        store.set("AIzaSyExampleKey123").unwrap();
        assert!(store.has_key());
        let again = KeyStore::open(path, None);
        assert_eq!(again.get(), "AIzaSyExampleKey123");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
