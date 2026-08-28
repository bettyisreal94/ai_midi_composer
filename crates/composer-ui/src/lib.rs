//! The plugin window's reusable UI state and rendering.
//!
//! A review after Phase 3 (see `REVIEW.md`) found that this crate was
//! an unused placeholder, even though `TODO.md` and `README.md`
//! already described it as owning the plugin window. All of Phase 3's
//! UI code lived in `composer-plugin` instead. This module fixes that.
//!
//! This crate does not talk to `nih_plug`, a host, the file system, or
//! the OS keychain. It only draws widgets, and reports back which
//! buttons the user pressed, as [`UiAction`] values. `composer-plugin` is
//! the one that creates the `nih_plug_egui` editor window, decides
//! what an action means (running the stub generator, opening a save
//! dialog, reading or writing the OS keychain), and owns any state a
//! host integration needs, such as a pending background task's request
//! ID.

use composer_core::midi::MidiClip;

/// The state shown by the status label next to "Generate" and "Vary
/// current clip". Also used for "Load .mid...", since loading a file
/// replaces the current clip the same way a generation result does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStatus {
    /// The user has not pressed "Generate", "Vary current clip", or
    /// "Load .mid..." yet this session.
    Idle,
    /// A generation request is running.
    Working,
    /// The last generation request, or file load, finished, and
    /// produced a clip.
    Done,
    /// The last generation request, or file load, failed, or the user
    /// gave bad input.
    Error,
}

/// Which AI provider shape the settings panel is configured for. Phase
/// 4 only defines these two client shapes in `composer-core`; see
/// `composer_core::provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI, DeepSeek, OpenRouter, Ollama, LM Studio, and any other
    /// server that accepts the same `/chat/completions` request shape.
    OpenAiCompatible,
    /// The Anthropic Claude Messages API.
    Anthropic,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 2] = [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic];

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
            ProviderKind::Anthropic => "Anthropic Claude",
        }
    }
}

/// The provider settings the user can edit. `composer-plugin` is
/// responsible for actually building an `AiProvider` from these
/// values, and for reading and writing `api_key` to the OS keychain;
/// this crate only holds the text the user typed.
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    /// Kept only in memory by this crate. Never logged. `composer-plugin`
    /// reads this to store it in the OS keychain, only when the user
    /// presses "Save API key", not on every keystroke.
    pub api_key: String,
}

impl ProviderSettings {
    /// A reasonable starting point for `kind`. The exact model name is
    /// just a starting point too: provider model names change often,
    /// and the user can always type a different one.
    pub fn defaults_for(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAiCompatible => Self {
                kind,
                base_url: "https://api.openai.com/v1".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: String::new(),
            },
            ProviderKind::Anthropic => Self {
                kind,
                base_url: composer_core::AnthropicProvider::DEFAULT_BASE_URL.to_string(),
                model: "claude-3-5-haiku-20241022".to_string(),
                api_key: String::new(),
            },
        }
    }
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self::defaults_for(ProviderKind::OpenAiCompatible)
    }
}

/// An action the user asked for by pressing a button. `composer-plugin`
/// decides what to actually do about it; this crate only reports that
/// it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// The user pressed "Generate": create a new clip from `prompt`
    /// alone.
    Generate,
    /// The user pressed "Vary current clip": create a new clip from
    /// the clip already loaded, plus `prompt` as the instruction for
    /// how to change it.
    GenerateVariation,
    /// The user pressed "Load .mid...".
    LoadMidFile,
    /// The user pressed "Save as .mid".
    Save,
    /// The user picked a different provider kind. `composer-plugin` should
    /// look up any API key already saved for the new kind, and fill
    /// `settings.api_key` with it (or clear it, if there is none).
    ProviderKindChanged,
    /// The user pressed "Save API key".
    SaveApiKey,
    /// The user pressed "Copy to clipboard" in the error details window.
    /// `composer-plugin` owns the actual OS clipboard (this crate has no
    /// OS dependency; see the module docs), so it reads
    /// `EditorState::error_detail` and writes it there.
    CopyErrorToClipboard,
}

/// The editor's own state: the prompt text, the last generation
/// result, the clip currently shown and saved, and the provider
/// settings.
pub struct EditorState {
    pub prompt: String,
    pub status: GenerationStatus,
    pub status_message: String,
    pub clip: MidiClip,
    /// The result of the last "Save as .mid" attempt: `Ok` with a
    /// short message on success, `Err` with the reason on failure.
    /// `None` before the user has pressed Save.
    pub save_message: Option<Result<String, String>>,
    pub settings: ProviderSettings,
    /// The result of the last "Save API key" attempt, shown the same
    /// way as `save_message`.
    pub settings_message: Option<Result<String, String>>,
    /// The full text of the most recent error, shown in its own
    /// floating window (see [`draw`]), separate from the short inline
    /// label next to whichever button triggered it. A maintainer asked
    /// for this after finding the inline label too easy to miss, and
    /// too small to read or copy from, while testing the plugin.
    /// `None` when there is nothing to show, or the user has closed the
    /// window. Sending this same text is what `UiAction::Save`,
    /// `UiAction::SaveApiKey`, and so on already do into
    /// `status_message`, `save_message`, or `settings_message`;
    /// `composer-plugin` sets this alongside those, at the same call
    /// site, whenever the message is an error.
    pub error_detail: Option<String>,
    /// The result of the last "Copy to clipboard" press in the error
    /// details window: `Some("Copied.")` on success, `Some(reason)` on
    /// failure. `None` before the button has been pressed for the
    /// error currently shown.
    pub clipboard_message: Option<Result<String, String>>,
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
            settings: ProviderSettings::default(),
            settings_message: None,
            error_detail: None,
            clipboard_message: None,
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
        ui.heading("AI MIDI Composer (dev)");
        ui.label(
            "\"Generate\" makes a new clip from the prompt below. \"Vary \
             current clip\" changes the loaded clip by the prompt's \
             instruction instead, such as \"add a harmony line\".",
        );
        ui.separator();

        egui::CollapsingHeader::new("Provider settings")
            .default_open(false)
            .show(ui, |ui| {
                let previous_kind = state.settings.kind;
                egui::ComboBox::from_label("Provider")
                    .selected_text(state.settings.kind.label())
                    .show_ui(ui, |ui| {
                        for kind in ProviderKind::ALL {
                            ui.selectable_value(&mut state.settings.kind, kind, kind.label());
                        }
                    });
                if state.settings.kind != previous_kind {
                    actions.push(UiAction::ProviderKindChanged);
                }

                ui.horizontal(|ui| {
                    ui.label("Base URL:");
                    ui.text_edit_singleline(&mut state.settings.base_url);
                });
                ui.horizontal(|ui| {
                    ui.label("Model:");
                    ui.text_edit_singleline(&mut state.settings.model);
                });
                ui.horizontal(|ui| {
                    ui.label("API key:");
                    ui.add(egui::TextEdit::singleline(&mut state.settings.api_key).password(true));
                });

                if ui.button("Save API key").clicked() {
                    actions.push(UiAction::SaveApiKey);
                }
                match &state.settings_message {
                    Some(Ok(message)) => {
                        ui.colored_label(egui::Color32::GREEN, message);
                    }
                    Some(Err(message)) => {
                        ui.colored_label(egui::Color32::RED, message);
                    }
                    None => {}
                }
            });

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

            let vary_button = egui::Button::new("Vary current clip");
            if ui.add_enabled(!generation_pending, vary_button).clicked() {
                actions.push(UiAction::GenerateVariation);
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
            "There is no drag-and-drop into or out of this window yet. \
             Use \"Load .mid...\" to open a file, and \"Save as .mid\" \
             then drag the saved file into your DAW.",
        );
        ui.horizontal(|ui| {
            let load_button = egui::Button::new("Load .mid...");
            if ui.add_enabled(!generation_pending, load_button).clicked() {
                actions.push(UiAction::LoadMidFile);
            }
            if ui.button("Save as .mid...").clicked() {
                actions.push(UiAction::Save);
            }
        });
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

    // A separate, dedicated window for the full text of the most recent
    // error, so it is not just a small inline label that is easy to
    // miss or hard to read and copy from. `egui::Window` is a floating
    // panel inside this plugin's own editor surface, not a second OS
    // window: `nih_plug_egui`'s windowing backend gives a plugin editor
    // exactly one OS window, with no supported way to open a second
    // one, so this is the closest equivalent available.
    if state.error_detail.is_some() {
        // `window_open` is egui's own "did the user click the window's
        // title-bar X" flag; it must stay the only thing written
        // through the `&mut` borrow `.open()` takes, since the borrow
        // checker will not allow the `show` closure below to also
        // assign to it directly (that would be two live mutable
        // borrows of the same variable at once). The "Close" button is
        // a separate, ordinary widget inside the window, not a second
        // way to drive that same flag, so it needs its own variable.
        let mut window_open = true;
        let mut close_clicked = false;
        egui::Window::new("Error details")
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .open(&mut window_open)
            .show(ctx, |ui| {
                let mut text_for_display = state.error_detail.clone().unwrap_or_default();
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text_for_display)
                                .interactive(false)
                                .desired_width(f32::INFINITY)
                                .desired_rows(10),
                        );
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Copy to clipboard").clicked() {
                        actions.push(UiAction::CopyErrorToClipboard);
                    }
                    if ui.button("Close").clicked() {
                        close_clicked = true;
                    }
                });
                match &state.clipboard_message {
                    Some(Ok(message)) => {
                        ui.colored_label(egui::Color32::GREEN, message);
                    }
                    Some(Err(message)) => {
                        ui.colored_label(egui::Color32::RED, message);
                    }
                    None => {}
                }
            });
        if !window_open || close_clicked {
            state.error_detail = None;
            state.clipboard_message = None;
        }
    }

    actions
}
