//! Core logic for the AI MIDI Agent plugin.
//!
//! This crate has no plugin code. Phase 0 only sets up the crate. Later
//! phases add the MIDI data model and the AI provider clients here.

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
