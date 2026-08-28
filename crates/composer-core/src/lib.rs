//! Core logic for the AI MIDI Composer plugin.
//!
//! This crate has no plugin code. It has the MIDI data model, in
//! [`midi`]; the AI provider clients, in [`provider`]; and the
//! prompt-to-MIDI pipeline that connects the two, in [`pipeline`].

pub mod midi;
pub mod pipeline;
pub mod provider;

pub use midi::{MidiClip, MidiError, Note, TimeSignature};
pub use pipeline::{generate_clip, generate_variation, parse_clip_reply, PipelineError};
pub use provider::{AiProvider, AnthropicProvider, OpenAiCompatibleProvider, ProviderError};

pub fn placeholder() -> &'static str {
    "composer-core"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_returns_name() {
        assert_eq!(placeholder(), "composer-core");
    }
}
