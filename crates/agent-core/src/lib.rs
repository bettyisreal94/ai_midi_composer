//! Core logic for the AI MIDI Agent plugin.
//!
//! This crate has no plugin code. It has the MIDI data model, in
//! [`midi`], and the AI provider clients, in [`provider`]. Later
//! phases add the prompt-to-MIDI pipeline here too.

pub mod midi;
pub mod provider;

pub use midi::{MidiClip, MidiError, Note, TimeSignature};
pub use provider::{AiProvider, AnthropicProvider, OpenAiCompatibleProvider, ProviderError};

pub fn placeholder() -> &'static str {
    "agent-core"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_returns_name() {
        assert_eq!(placeholder(), "agent-core");
    }
}
