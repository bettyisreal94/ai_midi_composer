//! Phase 5 plugin: a real prompt-to-MIDI pipeline.
//!
//! Phase 2 added a fixed, looping demo clip, sent as live MIDI output.
//! Phase 3 added the prompt text box, the "Generate" button, and the
//! status label, wired to a stub that always produced the same fixed
//! clip. Phase 4 added the AI provider clients, but did not connect
//! them to "Generate" yet. This phase makes that connection:
//! `background::GenerationStore` now runs `agent_core::generate_clip`,
//! a real call to whichever provider the settings panel is configured
//! for, and a freshly generated clip replaces the live-playing demo
//! clip, through [`clip_publisher`].
//!
//! The audio bus stays present and silent. VST3 has no clean "plugin
//! that only outputs MIDI" category, so this plugin presents itself as
//! an instrument instead, and simply does not fill its audio output.

mod background;
mod clip_publisher;
mod scheduler;
mod settings;

use std::sync::Arc;

use agent_core::midi::{MidiClip, Note, TimeSignature, DEFAULT_TICKS_PER_QUARTER};
use background::{GenerateTask, GenerationStore, ProviderConfig};
use clip_publisher::{ClipPublisher, ClipReader};
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, EguiState};
use scheduler::ActiveNotes;

/// Builds the fixed demo clip the plugin plays back before the user
/// generates anything: a one-octave, four note arpeggio (C4, E4, G4,
/// C5), one beat apart.
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

/// How long `clip` loops for, in ticks: the end of its last note. This
/// is a plugin-level concept, not part of the general [`MidiClip`] data
/// model, so it does not live in `agent-core`. A generated clip's
/// length is not fixed the way the demo clip's was, so this is
/// computed fresh from whatever clip is currently playing, not stored
/// as a fixed constant.
fn loop_length_ticks_for(clip: &MidiClip) -> u32 {
    clip.notes
        .iter()
        .map(|note| note.start.saturating_add(note.duration))
        .max()
        .unwrap_or(clip.ticks_per_quarter as u32 * 4)
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
    /// The editor's, and the background task executor's, side of the
    /// live-playback clip handoff. See `clip_publisher` for why this
    /// is a triple buffer, not a plain `MidiClip` field.
    clip_publisher: ClipPublisher,
    /// The audio thread's side of the same handoff. Only `process()`
    /// touches this.
    clip_reader: ClipReader,
    /// The generation number `process()` last saw from
    /// `clip_reader.read()`. Used to tell a genuine clip replacement
    /// apart from an unrelated transport update, so `process()` can
    /// clean up and resynchronize only when the clip itself changed.
    last_seen_generation: u64,
    /// The clip's current playback position, in ticks, wrapped to the
    /// current clip's own length (see `loop_length_ticks_for`). The
    /// audio thread owns this value.
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
        let (clip_publisher, clip_reader) = ClipPublisher::new(demo_clip());
        Self {
            params: Arc::new(AgentPluginParams::default()),
            egui_state: EguiState::from_size(360, 320),
            generation_store: GenerationStore::default(),
            clip_publisher,
            clip_reader,
            last_seen_generation: 0,
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
        let clip_publisher = self.clip_publisher.clone();

        let mut ui_state = agent_ui::EditorState::new(demo_clip());
        ui_state.settings.api_key = settings::load_api_key(ui_state.settings.kind);

        create_egui_editor(
            self.egui_state.clone(),
            PluginEditorState {
                ui: ui_state,
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
                                    format!("Generated {} notes.", clip.notes.len());
                                // Send the new clip to the audio thread
                                // for live playback, and show it in the
                                // editor for saving. See
                                // `clip_publisher` for why this is not
                                // a plain assignment shared with the
                                // audio thread.
                                clip_publisher.publish(clip.clone());
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
                                let config = ProviderConfig::from(&state.ui.settings);
                                let id = generation_store.submit(state.ui.prompt.clone(), config);
                                state.pending_request_id = Some(id);
                                state.ui.status = agent_ui::GenerationStatus::Working;
                                state.ui.status_message.clear();
                                async_executor.execute_background(GenerateTask(id));
                            }
                        }
                        agent_ui::UiAction::Save => {
                            state.ui.save_message = Some(save_clip_to_file(&state.ui.clip));
                        }
                        agent_ui::UiAction::ProviderKindChanged => {
                            // Show whatever key is already saved for
                            // the newly picked provider, instead of
                            // leaving the previous provider's key
                            // visible under the wrong provider.
                            let defaults =
                                agent_ui::ProviderSettings::defaults_for(state.ui.settings.kind);
                            state.ui.settings.api_key =
                                settings::load_api_key(state.ui.settings.kind);
                            state.ui.settings.base_url = defaults.base_url;
                            state.ui.settings.model = defaults.model;
                            state.ui.settings_message = None;
                        }
                        agent_ui::UiAction::SaveApiKey => {
                            state.ui.settings_message = Some(settings::save_api_key(
                                state.ui.settings.kind,
                                &state.ui.settings.api_key,
                            ));
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

        // `.read()` never allocates and never drops a value; see
        // `clip_publisher`. `published.generation` lets this tell a
        // genuine clip replacement (from a finished "Generate") apart
        // from an ordinary transport update.
        let published = self.clip_reader.read();
        let clip = &published.clip;
        let clip_changed = published.generation != self.last_seen_generation;
        self.last_seen_generation = published.generation;

        let loop_length_ticks = loop_length_ticks_for(clip);
        let loop_len_ticks = loop_length_ticks as f64;

        let tempo_bpm = transport.tempo.unwrap_or(clip.tempo_bpm);
        let samples_per_tick = if tempo_bpm > 0.0 {
            (60.0 / tempo_bpm) * self.sample_rate as f64 / clip.ticks_per_quarter as f64
        } else {
            0.0
        };
        if samples_per_tick <= 0.0 {
            return ProcessStatus::Normal;
        }

        // Detect seeks and other transport discontinuities from the
        // host's musical position (beats), not from
        // `pos_samples() / current tempo`. A project with an earlier
        // tempo change cannot be converted correctly with only the
        // current tempo, since samples-to-ticks depends on every tempo
        // that applied before this point, not just the current one
        // (see REVIEW.md). `pos_beats()` already accounts for that. A
        // clip replacement needs the same cleanup and resynchronizing
        // as a discontinuity: the notes that were sounding, and the
        // playhead position, belonged to a clip that is no longer
        // playing.
        let mut needs_resync = clip_changed;
        let mut host_tick = None;
        if let Some(beats) = transport.pos_beats() {
            let tick = beats * clip.ticks_per_quarter as f64;
            let jumped = match self.expected_host_tick {
                Some(expected) => (tick - expected).abs() > 0.5,
                None => true, // the first block since playback started
            };
            needs_resync = needs_resync || jumped;
            host_tick = Some(tick);
            self.expected_host_tick = Some(tick + num_samples as f64 / samples_per_tick);
        } else {
            // No musical position from the host. Let the internal
            // playhead run continuously when nothing else changed;
            // there is nothing more accurate available to resync it
            // to, or to compare it against to detect a jump.
            self.expected_host_tick = None;
        }

        let cleanup_events = if needs_resync {
            scheduler::stop_all_notes(&mut self.active_notes)
        } else {
            Vec::new()
        };
        if needs_resync {
            // Prefer the host's own position; fall back to the start
            // of the clip, rather than reusing a playhead position
            // that belonged to a different clip's timeline.
            self.playhead_ticks = host_tick.map_or(0.0, |tick| tick.rem_euclid(loop_len_ticks));
        }

        let (mut events, next_tick) = scheduler::schedule_clip_events(
            clip,
            loop_length_ticks,
            self.playhead_ticks,
            samples_per_tick,
            num_samples,
            &mut self.active_notes,
        );
        self.playhead_ticks = next_tick;

        // Cleanup note-offs from a discontinuity or a clip change
        // close out notes from before it, so they must play first.
        let mut all_events = cleanup_events;
        all_events.append(&mut events);
        emit_events(context, all_events);

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
