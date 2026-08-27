//! Reads and writes provider API keys to the OS's secure storage:
//! Keychain on macOS, Secret Service on Linux.
//!
//! `agent-ui` never touches the keychain, or even knows it exists; see
//! its module docs. This module is the only place in the project that
//! calls into `keyring`.

use agent_ui::ProviderKind;

const SERVICE_NAME: &str = "ai-midi-agent";

fn username_for(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAiCompatible => "openai-compatible",
        ProviderKind::Anthropic => "anthropic",
    }
}

fn entry_for(kind: ProviderKind) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE_NAME, username_for(kind)).map_err(|err| err.to_string())
}

/// Saves `api_key` for `kind` to the OS's secure storage, replacing any
/// key already saved for it. Returns a short message for the editor to
/// show.
pub fn save_api_key(kind: ProviderKind, api_key: &str) -> Result<String, String> {
    let entry = entry_for(kind)?;
    entry.set_password(api_key).map_err(|err| err.to_string())?;
    Ok("API key saved.".to_string())
}

/// Loads the API key saved for `kind`, if any. Returns an empty string
/// if none is saved, or if the OS's secure storage cannot be reached,
/// rather than failing: a missing key is a normal first-run state, not
/// an error the user needs to see.
pub fn load_api_key(kind: ProviderKind) -> String {
    entry_for(kind)
        .and_then(|entry| entry.get_password().map_err(|err| err.to_string()))
        .unwrap_or_default()
}
