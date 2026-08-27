//! Phase 3 plugin skeleton.
//!
//! This plugin does not call a real AI model yet. Phase 2 added a
//! fixed, looping demo clip, sent as live MIDI output. This phase adds
//! the prompt text box, the "Generate" button, and the status label,
//! but wires the button to a stub: it always produces the same fixed
//! clip, no matter what the prompt says. Phase 4 and Phase 5 replace
//! the stub with a real AI provider call. See `TODO.md`, Phase 3.
//!
//! The audio bus stays present and silent. VST3 has no clean "plugin
//! that only outputs MIDI" category, so this plugin presents itself as
//! an instrument instead, and simply does not fill its audio output.

use std::sync::Arc;

use agent_core::midi::{MidiClip, Note, TimeSignature, DEFAULT_TICKS_PER_QUARTER};
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, egui, EguiState};

/// Builds the fixed demo clip this phase plays back: a one-octave, four
/// note arpeggio (C4, E4, G4, C5), one beat apart, looping every 4
/// beats.
fn demo_clip() -> MidiClip {
    let beat = DEFAULT_TICKS_PER_QUARTER as u32;
    MidiClip {
        ticks_per_quarter: DEFAULT_TICKS_PER_QUARTER,
        tempo_bpm: 120.0,
        time_signature: TimeSignature::default(),
        notes: vec![
            Note {
                pitch: 60,
                velocity: 100,
                start: 0,
                duration: beat,
                channel: 0,
            },
            Note {
                pitch: 64,
                velocity: 100,
                start: beat,
                duration: beat,
                channel: 0,
            },
            Note {
                pitch: 67,
                velocity: 100,
                start: beat * 2,
                duration: beat,
                channel: 0,
            },
            Note {
                pitch: 72,
                velocity: 100,
                start: beat * 3,
                duration: beat,
                channel: 0,
            },
        ],
    }
}

/// How long the demo clip loops for, in ticks. This is a plugin-level
/// concept, not part of the general [`MidiClip`] data model, so it does
/// not live in `agent-core`.
fn demo_loop_length_ticks() -> u32 {
    DEFAULT_TICKS_PER_QUARTER as u32 * 4
}

/// The state shown by the "Generate" status label.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenerationStatus {
    /// The user has not pressed "Generate" yet this session.
    Idle,
    /// A generation request is running. Nothing sets this yet: the
    /// Phase 3 stub generator finishes inline, with no visible delay.
    /// Phase 4 and Phase 5 add a real network call, which sets this
    /// state while it runs.
    #[allow(dead_code)]
    Working,
    /// The last generation request finished, and produced a clip.
    Done,
    /// The last generation request failed, or the user gave bad input.
    Error,
}

/// The editor's own state: the prompt text, the last generation
/// result, and the clip currently shown and saved. This is separate
/// from [`AgentPlugin::clip`], which is the fixed clip the audio thread
/// plays back live. Phase 5 connects the two, by sending a freshly
/// generated clip to the audio thread; this phase does not, to keep
/// its scope small.
struct EditorState {
    prompt: String,
    status: GenerationStatus,
    status_message: String,
    clip: MidiClip,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            status: GenerationStatus::Idle,
            status_message: String::new(),
            clip: demo_clip(),
        }
    }
}

/// The stub behind the "Generate" button. A real prompt-to-MIDI call
/// (Phase 4 and Phase 5) will replace this. For now, it always returns
/// the same fixed clip, and only fails when the prompt is empty, so the
/// error path has a real, testable way to trigger.
fn generate_stub(prompt: &str) -> Result<MidiClip, &'static str> {
    if prompt.trim().is_empty() {
        return Err("Type a prompt first.");
    }
    Ok(demo_clip())
}

struct AgentPlugin {
    params: Arc<AgentPluginParams>,
    egui_state: Arc<EguiState>,
    clip: MidiClip,
    loop_length_ticks: u32,
    /// The clip's current playback position, in ticks, wrapped to
    /// `0..loop_length_ticks`. The audio thread owns this value.
    playhead_ticks: f64,
    /// The host's sample rate, set in `initialize()`.
    sample_rate: f32,
}

impl Default for AgentPlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(AgentPluginParams::default()),
            egui_state: EguiState::from_size(360, 240),
            clip: demo_clip(),
            loop_length_ticks: demo_loop_length_ticks(),
            playhead_ticks: 0.0,
            sample_rate: 44_100.0,
        }
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
    const MIDI_OUTPUT: MidiConfig = MidiConfig::Basic;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let loop_beats = self.loop_length_ticks / self.clip.ticks_per_quarter as u32;
        create_egui_editor(
            self.egui_state.clone(),
            EditorState::default(),
            |_, _| {},
            move |ctx, _setter, state| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("AI MIDI Agent (dev)");
                    ui.label(
                        "This is a Phase 3 skeleton. \"Generate\" always makes the \
                         same fixed clip. It does not call a real AI model yet.",
                    );
                    ui.separator();

                    ui.label("Prompt:");
                    ui.text_edit_multiline(&mut state.prompt);

                    ui.horizontal(|ui| {
                        if ui.button("Generate").clicked() {
                            // `generate_stub` runs to completion inline, so
                            // `GenerationStatus::Working` is never actually
                            // shown yet. It stays defined and handled below
                            // for Phase 4 and Phase 5, which replace this
                            // stub with a real network call that takes
                            // visible time.
                            match generate_stub(&state.prompt) {
                                Ok(clip) => {
                                    state.status_message =
                                        format!("Generated {} notes (stub).", clip.notes.len());
                                    state.clip = clip;
                                    state.status = GenerationStatus::Done;
                                }
                                Err(message) => {
                                    state.status_message = message.to_string();
                                    state.status = GenerationStatus::Error;
                                }
                            }
                        }

                        let (label, color) = match state.status {
                            GenerationStatus::Idle => ("idle".to_string(), egui::Color32::GRAY),
                            GenerationStatus::Working => {
                                ("working…".to_string(), egui::Color32::YELLOW)
                            }
                            GenerationStatus::Done => {
                                (state.status_message.clone(), egui::Color32::GREEN)
                            }
                            GenerationStatus::Error => {
                                (state.status_message.clone(), egui::Color32::RED)
                            }
                        };
                        ui.colored_label(color, label);
                    });

                    ui.separator();
                    ui.label(format!(
                        "Loaded clip: {} notes, {} BPM. The Phase 2 demo clip playing \
                         live still loops every {} beats, on its own; \"Generate\" does \
                         not change it yet (Phase 5 connects the two).",
                        state.clip.notes.len(),
                        state.clip.tempo_bpm as u32,
                        loop_beats,
                    ));
                    for note in &state.clip.notes {
                        ui.label(format!(
                            "  pitch {} · velocity {} · start tick {}",
                            note.pitch, note.velocity, note.start
                        ));
                    }

                    ui.separator();
                    ui.label(
                        "There is no drag-and-drop out of this window yet. Use \
                         \"Save as .mid\" and drag the file into your DAW instead.",
                    );
                    if ui.button("Save as .mid...").clicked() {
                        save_clip_to_file(&state.clip);
                    }
                });
            },
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.sample_rate = buffer_config.sample_rate;
        true
    }

    fn reset(&mut self) {
        self.playhead_ticks = 0.0;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // This plugin only outputs MIDI. Its audio bus stays silent.
        for channel_samples in buffer.iter_samples() {
            for sample in channel_samples {
                *sample = 0.0;
            }
        }

        let transport = context.transport();
        let num_samples = buffer.samples();
        if !transport.playing || num_samples == 0 {
            return ProcessStatus::Normal;
        }

        let tempo_bpm = transport.tempo.unwrap_or(self.clip.tempo_bpm);
        let samples_per_tick = if tempo_bpm > 0.0 {
            (60.0 / tempo_bpm) * self.sample_rate as f64 / self.clip.ticks_per_quarter as f64
        } else {
            0.0
        };
        if samples_per_tick <= 0.0 {
            return ProcessStatus::Normal;
        }

        let loop_len_ticks = self.loop_length_ticks as f64;

        // Keep the clip in sync with the host's own playback position,
        // when the host reports one. This stops the loop from drifting
        // out of sync after the user seeks the transport.
        if let Some(pos_samples) = transport.pos_samples() {
            self.playhead_ticks =
                (pos_samples as f64 / samples_per_tick).rem_euclid(loop_len_ticks);
        }

        let mut local_tick = self.playhead_ticks;
        let mut block_offset = 0u32;
        let mut remaining = num_samples;

        while remaining > 0 {
            let ticks_left_in_loop = (loop_len_ticks - local_tick).max(0.0);
            let samples_left_in_loop = (ticks_left_in_loop * samples_per_tick).ceil() as usize;
            let chunk = remaining.min(samples_left_in_loop.max(1));
            let tick_end = local_tick + chunk as f64 / samples_per_tick;

            for note in &self.clip.notes {
                let start = note.start as f64;
                let end = start + note.duration as f64;

                if start >= local_tick && start < tick_end {
                    let timing =
                        block_offset + ((start - local_tick) * samples_per_tick).round() as u32;
                    context.send_event(NoteEvent::NoteOn {
                        timing,
                        voice_id: None,
                        channel: note.channel,
                        note: note.pitch,
                        velocity: note.velocity as f32 / 127.0,
                    });
                }
                if end >= local_tick && end < tick_end {
                    let timing =
                        block_offset + ((end - local_tick) * samples_per_tick).round() as u32;
                    context.send_event(NoteEvent::NoteOff {
                        timing,
                        voice_id: None,
                        channel: note.channel,
                        note: note.pitch,
                        velocity: 0.0,
                    });
                }
            }

            remaining -= chunk;
            block_offset += chunk as u32;
            local_tick += chunk as f64 / samples_per_tick;
            if local_tick >= loop_len_ticks {
                local_tick -= loop_len_ticks;
            }
        }
        self.playhead_ticks = local_tick;

        ProcessStatus::Normal
    }
}

/// Opens a native "save file" dialog, and writes `clip` to the chosen
/// path as a standard MIDI file. Does nothing if the user cancels the
/// dialog. Logs failures with `nih_log!`, rather than the status label,
/// since a save error is rare enough not to need its own UI state yet.
fn save_clip_to_file(clip: &MidiClip) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("MIDI file", &["mid", "midi"])
        .set_file_name("ai-midi-agent-demo.mid")
        .save_file()
    else {
        return;
    };

    match clip.to_smf_bytes() {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(&path, bytes) {
                nih_log!("could not write MIDI file to {}: {err}", path.display());
            }
        }
        Err(err) => nih_log!("could not encode the demo clip as MIDI: {err}"),
    }
}

impl ClapPlugin for AgentPlugin {
    const CLAP_ID: &'static str = "com.example.ai-midi-agent";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Open source AI MIDI generator (development build)");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::Instrument, ClapFeature::Stereo];
}

impl Vst3Plugin for AgentPlugin {
    const VST3_CLASS_ID: [u8; 16] = *b"AiMidiAgentDev00";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Instrument];
}

nih_export_clap!(AgentPlugin);
nih_export_vst3!(AgentPlugin);
