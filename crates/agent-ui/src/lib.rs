//! The plugin window's reusable UI state and rendering.
//!
//! A review after Phase 3 (see `REVIEW.md`) found that this crate was
//! an unused placeholder, even though `TODO.md` and `README.md`
//! already described it as owning the plugin window. All of Phase 3's
//! UI code lived in `agent-plugin` instead. This module fixes that.
//!
//! This crate does not talk to `nih_plug`, a host, the file system, or
//! the OS keychain. It only draws widgets, and reports back which
//! buttons the user pressed, as [`UiAction`] values. `agent-plugin` is
//! the one that creates the `nih_plug_egui` editor window, decides
//! what an action means (running the stub generator, opening a save
//! dialog, reading or writing the OS keychain), and owns any state a
//! host integration needs, such as a pending background task's request
//! ID.

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

/// Which AI provider shape the settings panel is configured for. Phase
/// 4 only defines these two client shapes in `agent-core`; see
/// `agent_core::provider`.
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

/// The provider settings the user can edit. `agent-plugin` is
/// responsible for actually building an `AiProvider` from these
/// values, and for reading and writing `api_key` to the OS keychain;
/// this crate only holds the text the user typed.
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    /// Kept only in memory by this crate. Never logged. `agent-plugin`
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
                base_url: agent_core::AnthropicProvider::DEFAULT_BASE_URL.to_string(),
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

/// An action the user asked for by pressing a button. `agent-plugin`
/// decides what to actually do about it; this crate only reports that
/// it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// The user pressed "Generate".
    Generate,
    /// The user pressed "Save as .mid".
    Save,
    /// The user picked a different provider kind. `agent-plugin` should
    /// look up any API key already saved for the new kind, and fill
    /// `settings.api_key` with it (or clear it, if there is none).
    ProviderKindChanged,
    /// The user pressed "Save API key".
    SaveApiKey,
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
            "This is a Phase 4 skeleton. \"Generate\" still always makes the \
             same fixed clip: the provider settings below are not wired to \
             it yet. Phase 5 builds that connection.",
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
