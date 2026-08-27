//! The prompt-to-MIDI pipeline: turns a user's text prompt into a
//! [`MidiClip`], using an [`AiProvider`].
//!
//! This is the module that actually asks a model for notes. The
//! provider trait itself (in [`crate::provider`]) knows nothing about
//! MIDI, or JSON; it only sends text and gets text back. This module
//! is the one place that defines the JSON reply format, and turns a
//! reply into a `MidiClip`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::midi::{MidiClip, Note, TimeSignature, DEFAULT_TICKS_PER_QUARTER};
use crate::provider::{AiProvider, ProviderError};

/// The instructions sent to the model, before the user's own prompt.
/// Fixes the JSON reply shape the rest of this module expects.
///
/// Note timing uses beats (quarter notes), not ticks: a model reasons
/// about music in beats far more reliably than in an arbitrary tick
/// resolution, so this module converts beats to ticks itself, using
/// [`DEFAULT_TICKS_PER_QUARTER`].
const SYSTEM_PROMPT: &str = r#"You are a MIDI composition assistant.

Reply with a single JSON object, and nothing else. No explanation, no
Markdown code fence, just the JSON object, in exactly this shape:

{
  "tempo_bpm": 120,
  "time_signature_numerator": 4,
  "time_signature_denominator": 4,
  "notes": [
    {"pitch": 60, "velocity": 100, "start_beat": 0.0, "duration_beats": 1.0, "channel": 0}
  ]
}

Field rules:
- "pitch" is a MIDI note number, 0 to 127. Middle C is 60.
- "velocity" is how hard the note is played, 1 to 127. It cannot be 0.
- "start_beat" is when the note starts, in quarter notes from the start
  of the clip. 0 is the very start.
- "duration_beats" is the note's length, in quarter notes. It must be
  greater than 0.
- "channel" is 0 to 15. Use 0 unless there is a clear reason not to.
- "time_signature_denominator" must be a power of two, such as 2, 4,
  or 8.

Write at least one note. Do not let two notes on the same channel and
pitch overlap in time."#;

/// An error from [`generate_clip`].
#[derive(Debug)]
pub enum PipelineError {
    /// The provider call itself failed: no connection, a bad API key,
    /// and so on. Not retried; see [`generate_clip`]'s docs for why.
    Provider(ProviderError),
    /// The reply's content could not be turned into a `MidiClip`, even
    /// after one retry. Holds a message describing both attempts.
    BadReply(String),
    /// [`generate_variation`] was given a clip that is not itself
    /// valid, or that could not be described as JSON. Asking a model
    /// to vary a clip that does not make sense would not be
    /// meaningful.
    InvalidInput(String),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::Provider(err) => write!(f, "{err}"),
            PipelineError::BadReply(message) => write!(f, "{message}"),
            PipelineError::InvalidInput(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<ProviderError> for PipelineError {
    fn from(err: ProviderError) -> Self {
        PipelineError::Provider(err)
    }
}

#[derive(Serialize, Deserialize)]
struct RawClip {
    tempo_bpm: f64,
    #[serde(default = "default_numerator")]
    time_signature_numerator: u8,
    #[serde(default = "default_denominator")]
    time_signature_denominator: u8,
    notes: Vec<RawNote>,
}

fn default_numerator() -> u8 {
    4
}

fn default_denominator() -> u8 {
    4
}

#[derive(Serialize, Deserialize)]
struct RawNote {
    pitch: u8,
    velocity: u8,
    start_beat: f64,
    duration_beats: f64,
    #[serde(default)]
    channel: u8,
}

/// Describes `clip` in the same beats-based JSON shape
/// [`parse_clip_reply`] reads, so it can be shown back to a model, for
/// example inside [`generate_variation`]'s prompt.
fn clip_to_beats_json(clip: &MidiClip) -> Result<String, serde_json::Error> {
    let ticks_per_quarter = clip.ticks_per_quarter as f64;
    let raw = RawClip {
        tempo_bpm: clip.tempo_bpm,
        time_signature_numerator: clip.time_signature.numerator,
        time_signature_denominator: clip.time_signature.denominator,
        notes: clip
            .notes
            .iter()
            .map(|note| RawNote {
                pitch: note.pitch,
                velocity: note.velocity,
                start_beat: note.start as f64 / ticks_per_quarter,
                duration_beats: note.duration as f64 / ticks_per_quarter,
                channel: note.channel,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&raw)
}

/// Pulls the JSON object out of a reply, tolerating the small
/// formatting slips a model commonly makes despite being told not to:
/// a Markdown code fence around the object, or text before or after
/// it. Returns an error naming the problem if no `{...}` object can be
/// found at all.
fn extract_json(reply: &str) -> Result<&str, String> {
    let trimmed = reply.trim();
    let start = trimmed
        .find('{')
        .ok_or_else(|| "the reply has no JSON object".to_string())?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| "the reply has no JSON object".to_string())?;
    if end < start {
        return Err("the reply has no JSON object".to_string());
    }
    Ok(&trimmed[start..=end])
}

/// Parses one model reply into a `MidiClip`. Public so it can be
/// tested, and used, on its own, separately from [`generate_clip`]'s
/// retry behavior.
///
/// Converting a beat position or duration into ticks is lenient: a
/// negative `start_beat`, for example, becomes tick 0, rather than an
/// error. [`MidiClip::validate`] is what actually decides whether the
/// result is usable, the same way it decides for every other source of
/// a `MidiClip` in this project.
pub fn parse_clip_reply(reply: &str) -> Result<MidiClip, String> {
    let json_text = extract_json(reply)?;
    let raw: RawClip =
        serde_json::from_str(json_text).map_err(|err| format!("could not parse JSON: {err}"))?;

    if raw.notes.is_empty() {
        return Err("the reply had no notes".to_string());
    }

    let ticks_per_quarter = DEFAULT_TICKS_PER_QUARTER;
    let beats_to_ticks = |beats: f64| (beats * ticks_per_quarter as f64).round() as u32;

    let notes = raw
        .notes
        .into_iter()
        .map(|raw_note| Note {
            pitch: raw_note.pitch,
            velocity: raw_note.velocity,
            start: beats_to_ticks(raw_note.start_beat),
            duration: beats_to_ticks(raw_note.duration_beats),
            channel: raw_note.channel,
        })
        .collect();

    let clip = MidiClip {
        ticks_per_quarter,
        tempo_bpm: raw.tempo_bpm,
        time_signature: TimeSignature {
            numerator: raw.time_signature_numerator,
            denominator: raw.time_signature_denominator,
        },
        notes,
    };

    clip.validate()
        .map_err(|err| format!("the notes are not valid: {err}"))?;
    Ok(clip)
}

fn build_prompt(user_prompt: &str) -> String {
    format!("{SYSTEM_PROMPT}\n\nRequest: {user_prompt}")
}

fn build_retry_prompt(user_prompt: &str, previous_reply: &str, error_message: &str) -> String {
    format!(
        "{SYSTEM_PROMPT}\n\nRequest: {user_prompt}\n\n\
         Your last reply could not be used: {error_message}\n\n\
         Your last reply was:\n{previous_reply}\n\n\
         Reply again. Send only the JSON object, with nothing else."
    )
}

/// Turns `user_prompt` into a `MidiClip`, using `provider`.
///
/// If the first reply cannot be turned into a valid clip, this
/// function asks the model to try again once, this time telling it
/// what was wrong with its last reply. If the second attempt also
/// fails, it returns [`PipelineError::BadReply`], describing both
/// attempts.
///
/// A failure to reach the provider at all (`ProviderError`) is not
/// retried here: a broken connection or a bad API key will usually
/// fail the same way again immediately, and retrying it as if it were
/// a content problem would just mean a second, likely pointless,
/// network call. The caller decides whether to retry that case.
pub fn generate_clip(
    provider: &dyn AiProvider,
    user_prompt: &str,
) -> Result<MidiClip, PipelineError> {
    let first_reply = provider.complete(&build_prompt(user_prompt))?;
    let first_error = match parse_clip_reply(&first_reply) {
        Ok(clip) => return Ok(clip),
        Err(err) => err,
    };

    let retry_prompt = build_retry_prompt(user_prompt, &first_reply, &first_error);
    let second_reply = provider.complete(&retry_prompt)?;
    parse_clip_reply(&second_reply).map_err(|second_error| {
        PipelineError::BadReply(format!(
            "first attempt: {first_error}; after retry: {second_error}"
        ))
    })
}

/// Turns `existing_clip` plus `instruction` into a new `MidiClip`, such
/// as "add a harmony line" or "make a variation". Describes
/// `existing_clip` to the model as JSON, in the same shape a reply
/// must use, so the model can read it back directly.
///
/// This reuses [`generate_clip`]'s retry behavior: the "prompt" that
/// function sees is the existing clip plus the instruction, combined
/// into one piece of text. The rest of the pipeline, including the
/// JSON reply format, does not need to know the difference between
/// "generate something new" and "vary this clip".
///
/// Fails with [`PipelineError::InvalidInput`] if `existing_clip` is not
/// itself valid (see [`MidiClip::validate`]): asking a model to vary a
/// clip this project would reject on export would not be meaningful.
pub fn generate_variation(
    provider: &dyn AiProvider,
    existing_clip: &MidiClip,
    instruction: &str,
) -> Result<MidiClip, PipelineError> {
    existing_clip.validate().map_err(|err| {
        PipelineError::InvalidInput(format!("the existing clip is not valid: {err}"))
    })?;

    let existing_json = clip_to_beats_json(existing_clip).map_err(|err| {
        PipelineError::InvalidInput(format!("could not describe the existing clip: {err}"))
    })?;

    let user_prompt = format!(
        "Here is an existing clip, as JSON, in the same format you must \
         reply in:\n{existing_json}\n\nInstruction: {instruction}\n\n\
         Reply with the whole new clip, not just the changed notes, in \
         the same JSON format. You may keep, change, add, or remove \
         notes, to follow the instruction."
    );

    generate_clip(provider, &user_prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const VALID_REPLY: &str = r#"{
        "tempo_bpm": 120,
        "time_signature_numerator": 4,
        "time_signature_denominator": 4,
        "notes": [
            {"pitch": 60, "velocity": 100, "start_beat": 0.0, "duration_beats": 1.0, "channel": 0},
            {"pitch": 64, "velocity": 100, "start_beat": 1.0, "duration_beats": 1.0, "channel": 0}
        ]
    }"#;

    #[test]
    fn parses_a_well_formed_reply() {
        let clip = parse_clip_reply(VALID_REPLY).expect("should parse");
        assert_eq!(clip.notes.len(), 2);
        assert_eq!(clip.tempo_bpm, 120.0);
        assert_eq!(clip.notes[0].pitch, 60);
        assert_eq!(clip.notes[0].start, 0);
        assert_eq!(clip.notes[1].start, DEFAULT_TICKS_PER_QUARTER as u32);
    }

    #[test]
    fn strips_surrounding_text_and_a_markdown_code_fence() {
        let wrapped = format!("Sure, here you go:\n```json\n{VALID_REPLY}\n```\nEnjoy!");
        let clip = parse_clip_reply(&wrapped).expect("should parse despite the wrapping");
        assert_eq!(clip.notes.len(), 2);
    }

    #[test]
    fn rejects_a_reply_with_no_json_object() {
        assert!(parse_clip_reply("I cannot help with that.").is_err());
    }

    #[test]
    fn rejects_a_reply_with_no_notes() {
        let reply = r#"{"tempo_bpm": 120, "notes": []}"#;
        let err = parse_clip_reply(reply).expect_err("empty notes should be rejected");
        assert!(err.contains("no notes"));
    }

    #[test]
    fn rejects_notes_that_fail_clip_validation() {
        // Velocity 0: MidiClip::validate() rejects this as a silent
        // note.
        let reply = r#"{
            "tempo_bpm": 120,
            "notes": [
                {"pitch": 60, "velocity": 0, "start_beat": 0.0, "duration_beats": 1.0, "channel": 0}
            ]
        }"#;
        let err = parse_clip_reply(reply).expect_err("a silent note should be rejected");
        assert!(err.contains("not valid"));
    }

    /// A test double for `AiProvider`. Returns the next reply from a
    /// fixed list, in order, and counts how many times it was called,
    /// so tests can confirm `generate_clip` retries exactly once, not
    /// zero times or more than once.
    struct ScriptedProvider {
        replies: RefCell<Vec<Result<String, ProviderError>>>,
        call_count: RefCell<u32>,
        prompts_seen: RefCell<Vec<String>>,
    }

    impl ScriptedProvider {
        fn new(replies: Vec<Result<String, ProviderError>>) -> Self {
            Self {
                replies: RefCell::new(replies),
                call_count: RefCell::new(0),
                prompts_seen: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> u32 {
            *self.call_count.borrow()
        }

        fn last_prompt(&self) -> String {
            self.prompts_seen
                .borrow()
                .last()
                .cloned()
                .expect("complete() should have been called at least once")
        }
    }

    impl AiProvider for ScriptedProvider {
        fn complete(&self, prompt: &str) -> Result<String, ProviderError> {
            *self.call_count.borrow_mut() += 1;
            self.prompts_seen.borrow_mut().push(prompt.to_string());
            self.replies.borrow_mut().remove(0)
        }
    }

    #[test]
    fn generate_clip_succeeds_without_a_retry_on_a_good_first_reply() {
        let provider = ScriptedProvider::new(vec![Ok(VALID_REPLY.to_string())]);
        let clip = generate_clip(&provider, "a two note melody").expect("should succeed");
        assert_eq!(clip.notes.len(), 2);
        assert_eq!(provider.call_count(), 1);
    }

    #[test]
    fn generate_clip_retries_once_after_a_bad_first_reply() {
        let provider = ScriptedProvider::new(vec![
            Ok("not json at all".to_string()),
            Ok(VALID_REPLY.to_string()),
        ]);
        let clip = generate_clip(&provider, "a two note melody").expect("retry should succeed");
        assert_eq!(clip.notes.len(), 2);
        assert_eq!(provider.call_count(), 2);
    }

    #[test]
    fn generate_clip_fails_after_two_bad_replies() {
        let provider = ScriptedProvider::new(vec![
            Ok("not json at all".to_string()),
            Ok("still not json".to_string()),
        ]);
        let err =
            generate_clip(&provider, "a two note melody").expect_err("should fail after one retry");
        assert!(matches!(err, PipelineError::BadReply(_)));
        assert_eq!(provider.call_count(), 2);
    }

    #[test]
    fn generate_clip_does_not_retry_a_provider_error() {
        let provider = ScriptedProvider::new(vec![
            Err(ProviderError::Api {
                status: 401,
                message: "invalid API key".to_string(),
            }),
            Ok(VALID_REPLY.to_string()),
        ]);
        let err = generate_clip(&provider, "a two note melody")
            .expect_err("a provider error should not be retried here");
        assert!(matches!(err, PipelineError::Provider(_)));
        assert_eq!(
            provider.call_count(),
            1,
            "a provider error must not consume the retry attempt"
        );
    }

    fn existing_clip() -> MidiClip {
        MidiClip {
            ticks_per_quarter: DEFAULT_TICKS_PER_QUARTER,
            tempo_bpm: 90.0,
            time_signature: TimeSignature::default(),
            notes: vec![Note {
                pitch: 60,
                velocity: 100,
                start: 0,
                duration: DEFAULT_TICKS_PER_QUARTER as u32,
                channel: 0,
            }],
        }
    }

    #[test]
    fn clip_to_beats_json_then_parse_clip_reply_round_trips_notes() {
        let clip = existing_clip();
        let json = clip_to_beats_json(&clip).expect("should serialize");
        let round_tripped = parse_clip_reply(&json).expect("should parse its own output");
        assert_eq!(round_tripped.notes, clip.notes);
        assert_eq!(round_tripped.tempo_bpm, clip.tempo_bpm);
    }

    #[test]
    fn generate_variation_embeds_the_existing_clip_and_the_instruction() {
        let provider = ScriptedProvider::new(vec![Ok(VALID_REPLY.to_string())]);
        generate_variation(&provider, &existing_clip(), "add a harmony line")
            .expect("should succeed");

        let prompt = provider.last_prompt();
        assert!(prompt.contains("add a harmony line"));
        // The existing clip's one note, pitch 60, must show up in the
        // JSON embedded in the prompt.
        assert!(prompt.contains("\"pitch\": 60"));
    }

    #[test]
    fn generate_variation_retries_the_same_way_generate_clip_does() {
        let provider = ScriptedProvider::new(vec![
            Ok("not json at all".to_string()),
            Ok(VALID_REPLY.to_string()),
        ]);
        let clip = generate_variation(&provider, &existing_clip(), "make a variation")
            .expect("retry should succeed");
        assert_eq!(clip.notes.len(), 2);
        assert_eq!(provider.call_count(), 2);
    }

    #[test]
    fn generate_variation_rejects_an_invalid_existing_clip() {
        let mut invalid_clip = existing_clip();
        invalid_clip.notes[0].duration = 0; // MidiClip::validate() rejects this
        let provider = ScriptedProvider::new(vec![Ok(VALID_REPLY.to_string())]);

        let err = generate_variation(&provider, &invalid_clip, "add a harmony line")
            .expect_err("an invalid existing clip should be rejected");
        assert!(matches!(err, PipelineError::InvalidInput(_)));
        assert_eq!(
            provider.call_count(),
            0,
            "an invalid existing clip should not reach the provider at all"
        );
    }
}
