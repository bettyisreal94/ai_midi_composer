//! The CLAP/VST3 plugin: host integration, the editor window, and the
//! real-time audio callback.
//!
//! Phase 2 added a fixed, looping demo clip, sent as live MIDI output.
//! Phase 3 added the prompt text box, the "Generate" button, and the
//! status label, wired to a stub that always produced the same fixed
//! clip. Phase 4 added the AI provider clients. Phase 5 connected them
//! to "Generate": `background::GenerationStore` runs
//! `composer_core::generate_clip`, a real call to whichever provider the
//! settings panel is configured for, and a freshly generated clip
//! replaces the live-playing demo clip, through [`clip_publisher`].
//! Phase 6 added "Load .mid..." ([`load_clip_from_file`]) and "Vary
//! current clip" (`composer_core::generate_variation`, through the same
//! `GenerationStore`), so a loaded file and a generated variation both
//! reach the editor, the save path, and live playback the same way a
//! fresh generation result does.
//!
//! The audio bus stays present and silent. VST3 has no clean "plugin
//! that only outputs MIDI" category, so this plugin presents itself as
//! an instrument instead, and simply does not fill its audio output.

mod background;
mod clip_publisher;
mod scheduler;
mod settings;

use std::io::Read;
use std::sync::Arc;

use background::{GenerateTask, GenerationStore, ProviderConfig};
use clip_publisher::{ClipPublisher, ClipReader};
use composer_core::midi::{MidiClip, Note, TimeSignature, DEFAULT_TICKS_PER_QUARTER};
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

/// The editor's full state: the reusable UI state from `composer-ui`, plus
/// the one piece of bookkeeping that only makes sense on the plugin
/// side of the crate boundary: which background request, if any, is
/// still pending. `composer-ui` never sees this field.
struct PluginEditorState {
    ui: composer_ui::EditorState,
    pending_request_id: Option<background::RequestId>,
}

struct ComposerPlugin {
    params: Arc<ComposerPluginParams>,
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
    /// current clip's own length. The audio thread owns this value.
    playhead_ticks: f64,
    /// An index into the current `PlayableClip`'s event list: the next
    /// event `process()` has not yet sent. Advances as playback moves
    /// forward; relocated with binary search, not a scan, whenever the
    /// playhead resynchronizes. See `scheduler::schedule_events`.
    cursor: usize,
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

impl Default for ComposerPlugin {
    fn default() -> Self {
        let (clip_publisher, clip_reader) = ClipPublisher::new(demo_clip());
        Self {
            params: Arc::new(ComposerPluginParams::default()),
            // A maintainer found the previous size (360x320) too small
            // for this editor's own content: the "Save as .mid..."
            // button could end up below the visible area, with no way
            // to scroll to it. `composer_ui::draw()` now wraps the
            // whole page in a scroll area as the real fix, so every
            // button stays reachable regardless of window size; this
            // larger size just means that is rarely needed in normal
            // use.
            egui_state: EguiState::from_size(420, 560),
            generation_store: GenerationStore::default(),
            clip_publisher,
            clip_reader,
            last_seen_generation: 0,
            playhead_ticks: 0.0,
            cursor: 0,
            active_notes: ActiveNotes::default(),
            expected_host_tick: None,
            sample_rate: 44_100.0,
        }
    }
}

#[derive(Params, Default)]
struct ComposerPluginParams {}

impl Plugin for ComposerPlugin {
    const NAME: &'static str = "AI MIDI Composer (dev)";
    const VENDOR: &'static str = "AI MIDI Composer Project";
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

        let mut ui_state = composer_ui::EditorState::new(demo_clip());
        // Restore whichever provider kind, base URL, and model the user
        // last saved, so a plugin restart does not reset them to
        // hard-coded defaults. Falls back to the defaults already set
        // by `EditorState::new` if no settings file exists yet, or it
        // cannot be read.
        if let Some(persisted) = settings::load_persisted_settings() {
            ui_state.settings = persisted;
        }
        match settings::load_api_key(ui_state.settings.kind, &ui_state.settings.base_url) {
            Ok(api_key) => ui_state.settings.api_key = api_key,
            Err(message) => {
                let full_message = format!("could not read the saved API key: {message}");
                ui_state.error_detail = Some(full_message.clone());
                ui_state.settings_message = Some(Err(full_message));
            }
        }

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
                                state.ui.status = composer_ui::GenerationStatus::Done;
                            }
                            Err(message) => {
                                state.ui.error_detail = Some(message.clone());
                                state.ui.status_message = message;
                                state.ui.status = composer_ui::GenerationStatus::Error;
                            }
                        }
                    }
                }

                let pending = state.pending_request_id.is_some();
                let actions = composer_ui::draw(ctx, &mut state.ui, pending);

                for action in actions {
                    match action {
                        composer_ui::UiAction::Generate => {
                            // Ignore extra clicks while a request is
                            // already running. `composer_ui::draw` also
                            // disables the button for this, so this
                            // check only matters if a click was
                            // already queued the instant before that
                            // happened.
                            if state.pending_request_id.is_none() {
                                let config = ProviderConfig::from(&state.ui.settings);
                                let id =
                                    generation_store.submit(state.ui.prompt.clone(), config, None);
                                state.pending_request_id = Some(id);
                                state.ui.status = composer_ui::GenerationStatus::Working;
                                state.ui.status_message.clear();
                                async_executor.execute_background(GenerateTask(id));
                            }
                        }
                        composer_ui::UiAction::GenerateVariation => {
                            if state.pending_request_id.is_none() {
                                if state.ui.prompt.trim().is_empty() {
                                    state.ui.error_detail =
                                        Some("Type an instruction first.".to_string());
                                    state.ui.status_message =
                                        "Type an instruction first.".to_string();
                                    state.ui.status = composer_ui::GenerationStatus::Error;
                                } else {
                                    let config = ProviderConfig::from(&state.ui.settings);
                                    let id = generation_store.submit(
                                        state.ui.prompt.clone(),
                                        config,
                                        Some(state.ui.clip.clone()),
                                    );
                                    state.pending_request_id = Some(id);
                                    state.ui.status = composer_ui::GenerationStatus::Working;
                                    state.ui.status_message.clear();
                                    async_executor.execute_background(GenerateTask(id));
                                }
                            }
                        }
                        composer_ui::UiAction::LoadMidFile => match load_clip_from_file() {
                            Ok(Some(clip)) => {
                                state.ui.status_message =
                                    format!("Loaded {} notes from file.", clip.notes.len());
                                clip_publisher.publish(clip.clone());
                                state.ui.clip = clip;
                                state.ui.status = composer_ui::GenerationStatus::Done;
                            }
                            Ok(None) => {
                                // The user cancelled the file picker.
                            }
                            Err(message) => {
                                state.ui.error_detail = Some(message.clone());
                                state.ui.status_message = message;
                                state.ui.status = composer_ui::GenerationStatus::Error;
                            }
                        },
                        composer_ui::UiAction::Save => {
                            let result = save_clip_to_file(&state.ui.clip);
                            if let Err(message) = &result {
                                state.ui.error_detail = Some(message.clone());
                            }
                            state.ui.save_message = Some(result);
                        }
                        composer_ui::UiAction::ProviderKindChanged => {
                            // Show whatever key is already saved for
                            // the newly picked provider, instead of
                            // leaving the previous provider's key
                            // visible under the wrong provider. The
                            // base URL must be updated first: the
                            // keychain entry is keyed on (kind,
                            // base_url), not on kind alone, so each
                            // OpenAI-compatible service (OpenAI,
                            // DeepSeek, OpenRouter, a local server, ...)
                            // can keep its own separate key.
                            let defaults =
                                composer_ui::ProviderSettings::defaults_for(state.ui.settings.kind);
                            state.ui.settings.base_url = defaults.base_url;
                            state.ui.settings.model = defaults.model;
                            state.ui.settings_message = match settings::load_api_key(
                                state.ui.settings.kind,
                                &state.ui.settings.base_url,
                            ) {
                                Ok(api_key) => {
                                    state.ui.settings.api_key = api_key;
                                    None
                                }
                                Err(message) => {
                                    state.ui.settings.api_key = String::new();
                                    let full_message =
                                        format!("could not read the saved API key: {message}");
                                    state.ui.error_detail = Some(full_message.clone());
                                    Some(Err(full_message))
                                }
                            };
                        }
                        composer_ui::UiAction::SaveApiKey => {
                            let key_result = settings::save_api_key(
                                state.ui.settings.kind,
                                &state.ui.settings.base_url,
                                &state.ui.settings.api_key,
                            );
                            // Also persist the non-secret settings
                            // (kind, base URL, model) alongside the key,
                            // so they survive a plugin restart too. Best
                            // effort: a failure here should not hide
                            // that the API key itself did save.
                            if key_result.is_ok() {
                                let _ = settings::save_persisted_settings(&state.ui.settings);
                            }
                            if let Err(message) = &key_result {
                                state.ui.error_detail = Some(message.clone());
                            }
                            state.ui.settings_message = Some(key_result);
                        }
                        composer_ui::UiAction::CopyErrorToClipboard => {
                            // `composer-ui` has no OS dependency (see
                            // its module docs), so it only reports that
                            // the button was pressed; this is the one
                            // place that actually reaches the OS
                            // clipboard, through `arboard`, which talks
                            // to the OS directly and does not depend on
                            // `nih_plug_egui`'s own windowing backend
                            // having clipboard support.
                            let text = state.ui.error_detail.clone().unwrap_or_default();
                            state.ui.clipboard_message = Some(copy_to_clipboard(&text));
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
        self.cursor = 0;
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

        // Read every field this function needs from the transport up
        // front, into plain, owned values. `context.transport()`
        // borrows `context`; ending that borrow immediately, instead
        // of holding `transport` alive across the function, is what
        // lets the code below also borrow `context` mutably (through
        // `emit_event`) to send events.
        let transport = context.transport();
        let playing = transport.playing;
        let tempo = transport.tempo;
        let pos_beats = transport.pos_beats();
        let num_samples = buffer.samples();

        if !playing || num_samples == 0 {
            // Stopping must not leave a note stuck on at the host.
            self.expected_host_tick = None;
            scheduler::stop_all_notes(&mut self.active_notes, &mut |event| {
                emit_event(context, event)
            });
            return ProcessStatus::Normal;
        }

        // `.read()` never allocates and never drops a value; see
        // `clip_publisher`. `published.generation` lets this tell a
        // genuine clip replacement (from a finished "Generate") apart
        // from an ordinary transport update. `published.playable` is
        // already a sorted, ready-to-walk event list: nothing here
        // scans every note or sorts anything, unlike the version of
        // this function a review found still allocating and sorting
        // on the audio thread.
        let published = self.clip_reader.read();
        let playable = &published.playable;
        let clip_changed = published.generation != self.last_seen_generation;
        self.last_seen_generation = published.generation;

        if playable.is_empty() {
            return ProcessStatus::Normal;
        }
        let loop_len_ticks = playable.loop_length_ticks as f64;

        let tempo_bpm = tempo.unwrap_or(published.clip.tempo_bpm);
        let samples_per_tick = if tempo_bpm > 0.0 {
            (60.0 / tempo_bpm) * self.sample_rate as f64 / published.clip.ticks_per_quarter as f64
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
        if let Some(beats) = pos_beats {
            let tick = beats * published.clip.ticks_per_quarter as f64;
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

        if needs_resync {
            scheduler::stop_all_notes(&mut self.active_notes, &mut |event| {
                emit_event(context, event)
            });
            // Prefer the host's own position; fall back to the start
            // of the clip, rather than reusing a playhead position
            // that belonged to a different clip's timeline.
            self.playhead_ticks = host_tick.map_or(0.0, |tick| tick.rem_euclid(loop_len_ticks));
            // The cursor must move with the playhead: relocated with
            // binary search, not a scan, so a resync never costs more
            // than a normal block.
            self.cursor = playable.index_at_or_after(self.playhead_ticks);
        }

        let next_tick = scheduler::schedule_events(
            playable,
            &mut self.cursor,
            self.playhead_ticks,
            samples_per_tick,
            num_samples,
            &mut self.active_notes,
            &mut |event| emit_event(context, event),
        );
        self.playhead_ticks = next_tick;

        ProcessStatus::Normal
    }
}

/// Converts one [`scheduler::ScheduledEvent`] into the `NoteEvent`
/// type `nih_plug` expects, and sends it to the host.
fn emit_event(context: &mut impl ProcessContext<ComposerPlugin>, event: scheduler::ScheduledEvent) {
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

/// Writes `text` to the OS clipboard, using `arboard`, which talks to
/// the OS clipboard directly. This does not depend on
/// `nih_plug_egui`'s own windowing backend (`egui-baseview`) having its
/// own clipboard support, which this project has not confirmed either
/// way.
///
/// Opens a fresh clipboard handle for this one write, rather than
/// keeping one open for the plugin's whole lifetime: this is only ever
/// called from a single button press, not from any hot path, so the
/// small extra cost of opening it each time is not worth the added
/// state.
fn copy_to_clipboard(text: &str) -> Result<String, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|err| format!("could not reach the clipboard: {err}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|err| format!("could not copy to the clipboard: {err}"))?;
    Ok("Copied to clipboard.".to_string())
}

/// Opens a native "save file" dialog, and writes `clip` to the chosen
/// path as a standard MIDI file. Returns a short message describing
/// what happened, so the editor can show it. Returns `Ok` with an
/// empty message if the user cancels the dialog, since that is not a
/// failure.
fn save_clip_to_file(clip: &MidiClip) -> Result<String, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("MIDI file", &["mid", "midi"])
        .set_file_name("ai-midi-composer-demo.mid")
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

/// A chosen file larger than this is rejected outright, before its
/// bytes are even read into memory. A real MIDI file worth importing is
/// at most a few hundred kilobytes; this is generous headroom above
/// that. A review found that the load path read a whole chosen file
/// with no limit at all, on the UI thread, which meant an accidental or
/// hostile multi-gigabyte file could stall the editor and exhaust
/// memory. This does not move the read itself off the UI thread (a
/// picked file is normally small enough that the read is fast; see
/// `TODO.md` for why the bigger move-to-a-background-task change is
/// tracked separately), but it does stop an oversized file from ever
/// being read into memory in the first place.
const MAX_MIDI_FILE_BYTES: u64 = 5_000_000;

/// Opens a native "open file" dialog, and reads the chosen path as a
/// standard MIDI file, using the Phase 1 code
/// (`MidiClip::from_smf_bytes`). Returns `Ok(None)` if the user cancels
/// the dialog, since that is not a failure.
fn load_clip_from_file() -> Result<Option<MidiClip>, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("MIDI file", &["mid", "midi"])
        .pick_file()
    else {
        return Ok(None);
    };

    let metadata = std::fs::metadata(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    if metadata.len() > MAX_MIDI_FILE_BYTES {
        return Err(format!(
            "{} is {} bytes; the limit for a MIDI file is {MAX_MIDI_FILE_BYTES} bytes",
            path.display(),
            metadata.len()
        ));
    }

    // Bound the actual read too, not just the metadata check: a file
    // can grow between the check above and this read, and a special
    // file (for example a named pipe) can report a misleading size, or
    // none at all.
    let file = std::fs::File::open(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_MIDI_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    if bytes.len() as u64 > MAX_MIDI_FILE_BYTES {
        return Err(format!(
            "{} is larger than the {MAX_MIDI_FILE_BYTES}-byte limit",
            path.display()
        ));
    }

    let clip = MidiClip::from_smf_bytes(&bytes)
        .map_err(|err| format!("could not read {} as MIDI: {err}", path.display()))?;
    Ok(Some(clip))
}

impl ClapPlugin for ComposerPlugin {
    const CLAP_ID: &'static str = "com.example.ai-midi-composer";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Open source AI MIDI generator (development build)");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::Instrument, ClapFeature::Stereo];
}

impl Vst3Plugin for ComposerPlugin {
    const VST3_CLASS_ID: [u8; 16] = *b"AiMidiComposerD0";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Instrument];
}

nih_export_clap!(ComposerPlugin);
nih_export_vst3!(ComposerPlugin);
