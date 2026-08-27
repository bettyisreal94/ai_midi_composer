//! Core logic for the AI MIDI Agent plugin.
//!
//! This crate has no plugin code. It has the MIDI data model, in
//! [`midi`]; the AI provider clients, in [`provider`]; and the
//! prompt-to-MIDI pipeline that connects the two, in [`pipeline`].

pub mod midi;
pub mod pipeline;
pub mod provider;

pub use midi::{MidiClip, MidiError, Note, TimeSignature};
pub use pipeline::{generate_clip, parse_clip_reply, PipelineError};
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
