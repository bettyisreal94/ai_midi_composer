//! The plugin window's reusable UI state and rendering.
//!
//! A review after Phase 3 (see `REVIEW.md`) found that this crate was
//! an unused placeholder, even though `TODO.md` and `README.md`
//! already described it as owning the plugin window. All of Phase 3's
//! UI code lived in `agent-plugin` instead. This module fixes that.
//!
//! This crate does not talk to `nih_plug`, a host, or the file system.
//! It only draws widgets, and reports back which buttons the user
//! pressed, as [`UiAction`] values. `agent-plugin` is the one that
//! creates the `nih_plug_egui` editor window, decides what an action
//! means (running the stub generator, opening a save dialog), and owns
//! any state a host integration needs, such as a pending background
//! task's request ID.

use agent_core::midi::MidiClip;

/// The state shown by the "Generate" status label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStatus {
    /// The user has not pressed "Generate" yet this session.
    Idle,
    /// A generation request is running.
    Working,
    /// The last generation request finished, and produced a clip.
    Done,
    /// The last generation request failed, or the user gave bad input.
    Error,
}

/// An action the user asked for by pressing a button. `agent-plugin`
/// decides what to actually do about it; this crate only reports that
/// it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// The user pressed "Generate".
    Generate,
    /// The user pressed "Save as .mid".
    Save,
}

/// The editor's own state: the prompt text, the last generation
/// result, and the clip currently shown and saved.
pub struct EditorState {
    pub prompt: String,
    pub status: GenerationStatus,
    pub status_message: String,
    pub clip: MidiClip,
    /// The result of the last "Save as .mid" attempt: `Ok` with a
    /// short message on success, `Err` with the reason on failure.
    /// `None` before the user has pressed Save.
    pub save_message: Option<Result<String, String>>,
}

impl EditorState {
    /// Starts a new editor state, showing `initial_clip` before the
    /// user has generated or loaded anything else.
    pub fn new(initial_clip: MidiClip) -> Self {
        Self {
            prompt: String::new(),
            status: GenerationStatus::Idle,
            status_message: String::new(),
            clip: initial_clip,
            save_message: None,
        }
    }
}

/// Draws the plugin window's contents, and returns the actions the
/// user asked for this frame (usually none, sometimes one).
///
/// `generation_pending` disables the "Generate" button, so the user
/// cannot start a second request while one is already running.
pub fn draw(
    ctx: &egui::Context,
    state: &mut EditorState,
    generation_pending: bool,
) -> Vec<UiAction> {
    let mut actions = Vec::new();

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
            let generate_button = egui::Button::new("Generate");
            if ui
                .add_enabled(!generation_pending, generate_button)
                .clicked()
            {
                actions.push(UiAction::Generate);
            }

            let (label, color) = match state.status {
                GenerationStatus::Idle => ("idle".to_string(), egui::Color32::GRAY),
                GenerationStatus::Working => ("working…".to_string(), egui::Color32::YELLOW),
                GenerationStatus::Done => (state.status_message.clone(), egui::Color32::GREEN),
                GenerationStatus::Error => (state.status_message.clone(), egui::Color32::RED),
            };
            ui.colored_label(color, label);
        });

        ui.separator();
        ui.label(format!(
            "Loaded clip: {} notes, {} BPM.",
            state.clip.notes.len(),
            state.clip.tempo_bpm as u32,
        ));
        // A generated clip could have many notes. Keep the window a
        // fixed size by scrolling the note list, instead of growing
        // the window or spilling past its edge.
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                for note in &state.clip.notes {
                    ui.label(format!(
                        "  pitch {} · velocity {} · start tick {}",
                        note.pitch, note.velocity, note.start
                    ));
                }
            });

        ui.separator();
        ui.label(
            "There is no drag-and-drop out of this window yet. Use \
             \"Save as .mid\" and drag the file into your DAW instead.",
        );
        if ui.button("Save as .mid...").clicked() {
            actions.push(UiAction::Save);
        }
        match &state.save_message {
            Some(Ok(message)) => {
                ui.colored_label(egui::Color32::GREEN, message);
            }
            Some(Err(message)) => {
                ui.colored_label(egui::Color32::RED, message);
            }
            None => {}
        }
    });

    actions
}
