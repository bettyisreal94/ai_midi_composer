//! Phase 3 plugin skeleton.
//!
//! This plugin does not call a real AI model yet. Phase 2 added a
//! fixed, looping demo clip, sent as live MIDI output. Phase 3 added
//! the prompt text box, the "Generate" button, and the status label,
//! wired to a stub that always produces the same fixed clip. A review
//! after Phase 3 (see `REVIEW.md`) found scheduler bugs and an
//! architecture gap, both fixed here: the live MIDI scheduler
//! (`scheduler.rs`) now tracks active notes and cleans them up, and
//! generation now runs through a request-ID based background task seam
//! (`background.rs`) instead of inline on the UI thread. The UI itself
//! moved to `agent-ui`, which this crate wires up to `nih_plug_egui`.
//! Phase 4 and Phase 5 still own replacing the stub with a real AI
//! provider call. See `TODO.md`, Phase 3 and Phase 4.
//!
//! The audio bus stays present and silent. VST3 has no clean "plugin
//! that only outputs MIDI" category, so this plugin presents itself as
//! an instrument instead, and simply does not fill its audio output.

mod background;
mod scheduler;

use std::sync::Arc;

use agent_core::midi::{MidiClip, Note, TimeSignature, DEFAULT_TICKS_PER_QUARTER};
use background::{GenerateTask, GenerationStore};
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, EguiState};
use scheduler::ActiveNotes;

/// Builds the fixed demo clip this phase plays back: a one-octave, four
/// note arpeggio (C4, E4, G4, C5), one beat apart, looping every 4
/// beats. Also used by the "Generate" stub in `background.rs`.
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

/// The editor's full state: the reusable UI state from `agent-ui`, plus
/// the one piece of bookkeeping that only makes sense on the plugin
/// side of the crate boundary: which background request, if any, is
/// still pending. `agent-ui` never sees this field.
struct PluginEditorState {
    ui: agent_ui::EditorState,
    pending_request_id: Option<background::RequestId>,
}

struct AgentPlugin {
    params: Arc<AgentPluginParams>,
    egui_state: Arc<EguiState>,
    generation_store: GenerationStore,
    clip: MidiClip,
    loop_length_ticks: u32,
    /// The clip's current playback position, in ticks, wrapped to
    /// `0..loop_length_ticks`. The audio thread owns this value.
    playhead_ticks: f64,
    /// Which notes the audio thread has sent a note-on for, with no
    /// note-off sent yet. Used to clean up on stop and on a detected
    /// transport discontinuity, so no note is left stuck on.
    active_notes: ActiveNotes,
    /// The absolute (not loop-wrapped) host tick position expected at
    /// the start of the next `process()` call, derived from the host's
    /// beat position. `None` when playback is stopped, or the host
    /// does not report a beat position. Used to detect seeks and other
    /// discontinuities; see `process()`.
    expected_host_tick: Option<f64>,
    /// The host's sample rate, set in `initialize()`.
    sample_rate: f32,
}

impl Default for AgentPlugin {
    fn default() -> Self {
        Self {
            params: Arc::new(AgentPluginParams::default()),
            egui_state: EguiState::from_size(360, 320),
            generation_store: GenerationStore::default(),
            clip: demo_clip(),
            loop_length_ticks: demo_loop_length_ticks(),
            playhead_ticks: 0.0,
            active_notes: ActiveNotes::default(),
            expected_host_tick: None,
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
    type BackgroundTask = GenerateTask;

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn task_executor(&mut self) -> TaskExecutor<Self> {
        let store = self.generation_store.clone();
        Box::new(move |GenerateTask(id)| store.run(id))
    }

    fn editor(&mut self, async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let generation_store = self.generation_store.clone();
        let initial_clip = self.clip.clone();

        create_egui_editor(
            self.egui_state.clone(),
            PluginEditorState {
                ui: agent_ui::EditorState::new(initial_clip),
                pending_request_id: None,
            },
            |_, _| {},
            move |ctx, _setter, state| {
                // Check for a finished background generation before
                // drawing, so this frame already shows the result.
                if let Some(id) = state.pending_request_id {
                    if let Some(result) = generation_store.poll(id) {
                        state.pending_request_id = None;
                        match result {
                            Ok(clip) => {
                                state.ui.status_message =
                                    format!("Generated {} notes (stub).", clip.notes.len());
                                state.ui.clip = clip;
                                state.ui.status = agent_ui::GenerationStatus::Done;
                            }
                            Err(message) => {
                                state.ui.status_message = message;
                                state.ui.status = agent_ui::GenerationStatus::Error;
                            }
                        }
                    }
                }

                let pending = state.pending_request_id.is_some();
                let actions = agent_ui::draw(ctx, &mut state.ui, pending);

                for action in actions {
                    match action {
                        agent_ui::UiAction::Generate => {
                            // Ignore extra clicks while a request is
                            // already running. `agent_ui::draw` also
                            // disables the button for this, so this
                            // check only matters if a click was
                            // already queued the instant before that
                            // happened.
                            if state.pending_request_id.is_none() {
                                let id = generation_store.submit(state.ui.prompt.clone());
                                state.pending_request_id = Some(id);
                                state.ui.status = agent_ui::GenerationStatus::Working;
                                state.ui.status_message.clear();
                                async_executor.execute_background(GenerateTask(id));
                            }
                        }
                        agent_ui::UiAction::Save => {
                            state.ui.save_message = Some(save_clip_to_file(&state.ui.clip));
                        }
                    }
                }
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
        // `reset()` has no `ProcessContext`, so it cannot send cleanup
        // note-offs itself. The host is expected to already treat this
        // as a full reinitialization; just clear this plugin's own
        // bookkeeping, so the next `process()` call starts clean.
        self.playhead_ticks = 0.0;
        self.active_notes = ActiveNotes::default();
        self.expected_host_tick = None;
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
            // Stopping must not leave a note stuck on at the host.
            self.expected_host_tick = None;
            emit_events(context, scheduler::stop_all_notes(&mut self.active_notes));
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

        // Detect seeks and other transport discontinuities from the
        // host's musical position (beats), not from
        // `pos_samples() / current tempo`. A project with an earlier
        // tempo change cannot be converted correctly with only the
        // current tempo, since samples-to-ticks depends on every tempo
        // that applied before this point, not just the current one
        // (see REVIEW.md). `pos_beats()` already accounts for that.
        let mut cleanup_events = Vec::new();
        if let Some(beats) = transport.pos_beats() {
            let host_tick = beats * self.clip.ticks_per_quarter as f64;
            let jumped = match self.expected_host_tick {
                Some(expected) => (host_tick - expected).abs() > 0.5,
                None => true, // the first block since playback started
            };
            if jumped {
                cleanup_events = scheduler::stop_all_notes(&mut self.active_notes);
                self.playhead_ticks = host_tick.rem_euclid(loop_len_ticks);
            }
            self.expected_host_tick = Some(host_tick + num_samples as f64 / samples_per_tick);
        } else {
            // No musical position from the host. Let the internal
            // playhead run continuously; there is nothing more
            // accurate available to resync it to, or to compare it
            // against to detect a jump.
            self.expected_host_tick = None;
        }

        let (mut events, next_tick) = scheduler::schedule_clip_events(
            &self.clip,
            self.loop_length_ticks,
            self.playhead_ticks,
            samples_per_tick,
            num_samples,
            &mut self.active_notes,
        );
        self.playhead_ticks = next_tick;

        // Cleanup note-offs from a discontinuity close out notes from
        // before the jump, so they must play first.
        cleanup_events.append(&mut events);
        emit_events(context, cleanup_events);

        ProcessStatus::Normal
    }
}

/// Sends `events` to the host, in order, converting each
/// [`scheduler::ScheduledEvent`] into the `NoteEvent` type `nih_plug`
/// expects.
fn emit_events(
    context: &mut impl ProcessContext<AgentPlugin>,
    events: Vec<scheduler::ScheduledEvent>,
) {
    for event in events {
        let note_event = match event.kind {
            scheduler::ScheduledEventKind::NoteOn { velocity } => NoteEvent::NoteOn {
                timing: event.timing,
                voice_id: None,
                channel: event.channel,
                note: event.pitch,
                velocity,
            },
            scheduler::ScheduledEventKind::NoteOff => NoteEvent::NoteOff {
                timing: event.timing,
                voice_id: None,
                channel: event.channel,
                note: event.pitch,
                velocity: 0.0,
            },
        };
        context.send_event(note_event);
    }
}

/// Opens a native "save file" dialog, and writes `clip` to the chosen
/// path as a standard MIDI file. Returns a short message describing
/// what happened, so the editor can show it. Returns `Ok` with an
/// empty message if the user cancels the dialog, since that is not a
/// failure.
fn save_clip_to_file(clip: &MidiClip) -> Result<String, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("MIDI file", &["mid", "midi"])
        .set_file_name("ai-midi-agent-demo.mid")
        .save_file()
    else {
        return Ok(String::new());
    };

    let bytes = clip
        .to_smf_bytes()
        .map_err(|err| format!("could not encode the clip as MIDI: {err}"))?;
    std::fs::write(&path, bytes)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(format!("Saved to {}.", path.display()))
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
