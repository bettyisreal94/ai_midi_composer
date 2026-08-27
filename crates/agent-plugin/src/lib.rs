//! Phase 0 plugin skeleton.
//!
//! This plugin does not generate MIDI yet. It only passes audio through,
//! unchanged. Its only job is to prove the CLAP and VST3 build and load
//! in a host on Linux and macOS. Later phases replace `process` with the
//! real MIDI generation code.

use nih_plug::prelude::*;
use std::sync::Arc;

struct AgentPlugin;

impl Default for AgentPlugin {
    fn default() -> Self {
        Self
    }
}

#[derive(Params, Default)]
struct AgentPluginParams {}

impl Plugin for AgentPlugin {
    const NAME: &'static str = "AI MIDI Agent (dev)";
    const VENDOR: &'static str = "AI MIDI Agent Project";
    const URL: &'static str = env!("CARGO_PKG_REPOSITORY");
    const EMAIL: &'static str = "none@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        Arc::new(AgentPluginParams::default())
    }

    fn process(
        &mut self,
        _buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Passthrough: NIH-plug hands us the input audio already in
        // `buffer`. We do not touch it, so input equals output.
        ProcessStatus::Normal
    }
}

impl ClapPlugin for AgentPlugin {
    const CLAP_ID: &'static str = "com.example.ai-midi-agent";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Open source AI MIDI generator (development build)");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Stereo];
}

impl Vst3Plugin for AgentPlugin {
    const VST3_CLASS_ID: [u8; 16] = *b"AiMidiAgentDev00";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx];
}

nih_export_clap!(AgentPlugin);
nih_export_vst3!(AgentPlugin);
