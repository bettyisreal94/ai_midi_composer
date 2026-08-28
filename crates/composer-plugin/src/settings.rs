//! Reads and writes provider settings: API keys in the OS's secure
//! storage (Keychain on macOS, Secret Service on Linux), and the
//! non-secret settings (provider kind, base URL, model) in a small JSON
//! file in the OS's normal config directory.
//!
//! `composer-ui` never touches the keychain, the file system, or even
//! knows either one exists; see its module docs. This module is the
//! only place in the project that calls into `keyring`, and the only
//! place that reads or writes the settings file.

use std::io::Read;

use composer_ui::{ProviderKind, ProviderSettings};
use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "ai-midi-composer";

/// A settings file larger than this is rejected outright, before it is
/// even parsed as JSON. This file holds three short strings; this is
/// generous headroom above that, not a realistic expectation. A limit
/// here, the same way `composer-core::provider` bounds a provider
/// response, stops a corrupted or hand-edited file from making this
/// project read an unbounded amount of memory.
const MAX_SETTINGS_FILE_BYTES: u64 = 100_000;

/// A username that is unique per exact service, not only per
/// [`ProviderKind`]. A review found that keying the keychain entry on
/// `kind` alone means every OpenAI-compatible service (OpenAI itself,
/// DeepSeek, OpenRouter, a local Ollama or LM Studio server, ...)
/// shares one saved key, so switching between them loses whichever key
/// was saved for the one used before. Folding `base_url` into the
/// username fixes this: each distinct base URL gets its own entry.
/// Anthropic has only one real base URL in practice, but this also
/// lets a user pointed at a proxy keep a separate key for it.
fn username_for(kind: ProviderKind, base_url: &str) -> String {
    let prefix = match kind {
        ProviderKind::OpenAiCompatible => "openai-compatible",
        ProviderKind::Anthropic => "anthropic",
    };
    let sanitized_base_url: String = base_url
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{prefix}:{sanitized_base_url}")
}

fn entry_for(kind: ProviderKind, base_url: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE_NAME, &username_for(kind, base_url)).map_err(|err| err.to_string())
}

/// Saves `api_key` for `kind` and `base_url` to the OS's secure storage,
/// replacing any key already saved for that exact pair. Refuses to save
/// an empty (or all-whitespace) key: a review found that the earlier
/// version happily overwrote a previously saved, valid key with an
/// empty one whenever the user pressed "Save API key" with the field
/// cleared, which is very easy to do by accident. Returns a short
/// message for the editor to show either way.
pub fn save_api_key(kind: ProviderKind, base_url: &str, api_key: &str) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("Type an API key before saving; an empty key was not saved.".to_string());
    }
    let entry = entry_for(kind, base_url)?;
    entry.set_password(api_key).map_err(|err| err.to_string())?;
    Ok("API key saved.".to_string())
}

/// Loads the API key saved for `kind` and `base_url`, if any.
///
/// Returns an empty string if none is saved yet: that is a normal
/// first-run state, not an error. A review found that the earlier
/// version also returned an empty string for every *other* keyring
/// failure (a locked keychain, no Secret Service running, and so on),
/// which looks identical to "no key saved" in the editor, and could
/// lead a user to re-type and re-save a key that was actually fine, or
/// to wrongly conclude a save had silently failed. This version reports
/// those failures as `Err`, so the editor can show them; only a
/// genuinely missing entry becomes `Ok(String::new())`.
pub fn load_api_key(kind: ProviderKind, base_url: &str) -> Result<String, String> {
    let entry = entry_for(kind, base_url)?;
    match entry.get_password() {
        Ok(password) => Ok(password),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(err) => Err(err.to_string()),
    }
}

/// The non-secret part of [`ProviderSettings`] (everything except
/// `api_key`, which lives in the keychain, never in a plain file), in
/// the shape written to, and read from, disk.
///
/// A review found that `kind`, `base_url`, and `model` were not
/// persisted at all: every plugin restart lost them, and reset to
/// [`ProviderSettings::defaults_for`]'s hard-coded starting point.
#[derive(Serialize, Deserialize)]
struct StoredSettings {
    kind: StoredProviderKind,
    base_url: String,
    model: String,
}

/// A serializable mirror of [`ProviderKind`]. `composer-ui` deliberately
/// has no `serde` dependency (see its module docs), so this project's
/// one JSON encoding of a provider kind lives here instead, next to the
/// only code that reads or writes the settings file.
#[derive(Serialize, Deserialize, Clone, Copy)]
enum StoredProviderKind {
    OpenAiCompatible,
    Anthropic,
}

impl From<ProviderKind> for StoredProviderKind {
    fn from(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAiCompatible => StoredProviderKind::OpenAiCompatible,
            ProviderKind::Anthropic => StoredProviderKind::Anthropic,
        }
    }
}

impl From<StoredProviderKind> for ProviderKind {
    fn from(kind: StoredProviderKind) -> Self {
        match kind {
            StoredProviderKind::OpenAiCompatible => ProviderKind::OpenAiCompatible,
            StoredProviderKind::Anthropic => ProviderKind::Anthropic,
        }
    }
}

fn settings_file_path() -> Result<std::path::PathBuf, String> {
    directories::ProjectDirs::from("", "", "ai-midi-composer")
        .map(|dirs| dirs.config_dir().join("settings.json"))
        .ok_or_else(|| "could not find a config directory on this OS".to_string())
}

/// Loads the non-secret provider settings saved by a previous run, if
/// any. Returns `None` if no settings file exists yet (a normal
/// first-run state), or if it cannot be read or parsed: a corrupted or
/// hand-edited settings file should fall back to defaults, not stop the
/// editor from opening. The caller still needs to load `api_key`
/// separately, with [`load_api_key`]; this never touches the keychain.
pub fn load_persisted_settings() -> Option<ProviderSettings> {
    let path = settings_file_path().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_SETTINGS_FILE_BYTES {
        return None;
    }
    let file = std::fs::File::open(&path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SETTINGS_FILE_BYTES {
        return None;
    }
    let stored: StoredSettings = serde_json::from_slice(&bytes).ok()?;
    Some(ProviderSettings {
        kind: stored.kind.into(),
        base_url: stored.base_url,
        model: stored.model,
        api_key: String::new(),
    })
}

/// Saves `settings`'s non-secret fields (`kind`, `base_url`, `model`) to
/// a small JSON file in the OS's normal config directory, so they
/// survive a plugin restart. Never writes `api_key`; that stays in the
/// keychain, saved separately by [`save_api_key`].
pub fn save_persisted_settings(settings: &ProviderSettings) -> Result<(), String> {
    let path = settings_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let stored = StoredSettings {
        kind: settings.kind.into(),
        base_url: settings.base_url.clone(),
        model: settings.model.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&stored).map_err(|err| err.to_string())?;
    std::fs::write(&path, bytes).map_err(|err| err.to_string())
}
