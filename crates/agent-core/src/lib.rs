//! Core logic for the AI MIDI Agent plugin.
//!
//! This crate has no plugin code. It has the MIDI data model, in
//! [`midi`]. Later phases add the AI provider clients here too.

pub mod midi;

pub use midi::{MidiClip, MidiError, Note, TimeSignature};

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
