//! The MIDI data model.
//!
//! A [`MidiClip`] holds a list of [`Note`] values, plus tempo and time
//! signature. This module also converts a [`MidiClip`] to the bytes of a
//! standard MIDI file, and back.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use midly::{
    num::{u15, u24, u28, u4, u7},
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track, TrackEvent, TrackEventKind,
};

/// The default resolution for new clips: ticks per quarter note.
pub const DEFAULT_TICKS_PER_QUARTER: u16 = 960;

/// One played note.
///
/// `start` and `duration` are in ticks. A tick is a fraction of a
/// quarter note. [`MidiClip::ticks_per_quarter`] says how many ticks
/// make one quarter note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    /// The MIDI pitch. Valid range: 0 to 127. Middle C is 60.
    pub pitch: u8,
    /// The note velocity, or how hard the note is played. Valid range:
    /// 1 to 127. MIDI treats a note-on with velocity 0 as a note-off,
    /// so a velocity of 0 cannot make an audible note.
    pub velocity: u8,
    /// The start time, in ticks from the start of the clip.
    pub start: u32,
    /// The note length, in ticks. Must be greater than 0.
    pub duration: u32,
    /// The MIDI channel. Valid range: 0 to 15.
    pub channel: u8,
}

/// A musical time signature, such as 4/4 or 3/4.
///
/// `denominator` must be a power of two, because that is what the
/// standard MIDI file format requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

/// A list of notes, plus the data needed to play them back correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiClip {
    /// How many ticks make one quarter note.
    pub ticks_per_quarter: u16,
    /// The playback tempo, in beats per minute. Must be finite and
    /// greater than 0.
    pub tempo_bpm: f64,
    pub time_signature: TimeSignature,
    pub notes: Vec<Note>,
}

impl Default for MidiClip {
    fn default() -> Self {
        Self {
            ticks_per_quarter: DEFAULT_TICKS_PER_QUARTER,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: Vec::new(),
        }
    }
}

/// An error from building or reading MIDI data.
#[derive(Debug)]
pub enum MidiError {
    /// A field does not fit the MIDI file format, such as a pitch above
    /// 127, or a time signature denominator that is not a power of two.
    OutOfRange { field: &'static str, value: u32 },
    /// A note has velocity 0. MIDI treats this as a note-off, so the
    /// note could never be heard.
    SilentNote,
    /// A note has duration 0.
    ZeroDuration,
    /// Two or more notes use the same channel and pitch, and overlap in
    /// time. Exporting this would produce a file where the note-on and
    /// note-off events cannot be matched back to the original notes
    /// without guessing, so this project rejects it instead.
    OverlappingNotes { channel: u8, pitch: u8 },
    /// The tempo is not finite, or not greater than 0.
    InvalidTempo(f64),
    /// The tempo is too slow or too fast to fit the MIDI file format.
    TempoOutOfRange,
    /// A gap between two events is too large to fit the MIDI file
    /// format.
    TickOverflow,
    /// The MIDI file bytes could not be parsed.
    Parse(midly::Error),
    /// The file uses a timing format this project does not support.
    UnsupportedTiming,
    /// The file uses format 2 (independent, sequential tracks). This
    /// project only reads formats 0 and 1, where every track is part
    /// of the same, simultaneous performance. Merging format 2's
    /// tracks the same way would wrongly play unrelated songs at once.
    UnsupportedFormat,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::OutOfRange { field, value } => {
                write!(f, "value {value} does not fit in field '{field}'")
            }
            MidiError::SilentNote => {
                write!(f, "a note has velocity 0, so it would be silent")
            }
            MidiError::ZeroDuration => write!(f, "a note has duration 0"),
            MidiError::OverlappingNotes { channel, pitch } => write!(
                f,
                "channel {channel} has two overlapping notes at pitch {pitch}"
            ),
            MidiError::InvalidTempo(bpm) => {
                write!(f, "tempo {bpm} beats per minute is not valid")
            }
            MidiError::TempoOutOfRange => {
                write!(f, "tempo does not fit the MIDI file format")
            }
            MidiError::TickOverflow => {
                write!(f, "a gap between events does not fit the MIDI file format")
            }
            MidiError::Parse(err) => write!(f, "could not parse the MIDI file: {err}"),
            MidiError::UnsupportedTiming => {
                write!(f, "this project only supports metrical (ticks) timing")
            }
            MidiError::UnsupportedFormat => {
                write!(
                    f,
                    "this project cannot read format 2 (independent sequential tracks)"
                )
            }
        }
    }
}

impl std::error::Error for MidiError {}

impl From<midly::Error> for MidiError {
    fn from(err: midly::Error) -> Self {
        MidiError::Parse(err)
    }
}

fn to_u7(field: &'static str, value: u8) -> Result<u7, MidiError> {
    u7::try_from(value).ok_or(MidiError::OutOfRange {
        field,
        value: value as u32,
    })
}

fn to_u4(field: &'static str, value: u8) -> Result<u4, MidiError> {
    u4::try_from(value).ok_or(MidiError::OutOfRange {
        field,
        value: value as u32,
    })
}

/// Finds the first pair of notes that share a channel and pitch, and
/// overlap in time. Returns `None` if there is no such pair.
///
/// This scans a `BTreeMap`, not a `HashMap`, so the result is the same
/// every time this function runs on the same input.
fn find_overlap(notes: &[Note]) -> Option<(u8, u8)> {
    let mut by_key: BTreeMap<(u8, u8), Vec<(u32, u32)>> = BTreeMap::new();
    for note in notes {
        let end = note.start.saturating_add(note.duration.max(1));
        by_key
            .entry((note.channel, note.pitch))
            .or_default()
            .push((note.start, end));
    }

    for (key, mut intervals) in by_key {
        intervals.sort_by_key(|&(start, _)| start);
        for pair in intervals.windows(2) {
            let (_, first_end) = pair[0];
            let (second_start, _) = pair[1];
            if second_start < first_end {
                return Some(key);
            }
        }
    }

    None
}

/// One entry in the flat event timeline used while writing a MIDI file.
struct TimedEvent {
    tick: u32,
    /// Events at the same tick play in this order: meta events first,
    /// then note-off events, then note-on events. This order stops a
    /// new note from being cut off by a note-off that belongs to an
    /// older note at the same tick.
    order: u8,
    kind: TrackEventKind<'static>,
}

impl MidiClip {
    /// Checks that every field of this clip fits the standard MIDI
    /// file format, without building the bytes. [`Self::to_smf_bytes`]
    /// calls this first. Also useful on its own, when only a yes/no
    /// answer is needed, such as after parsing a clip from an AI
    /// provider's reply.
    ///
    /// Returns an error for: an out-of-range pitch, velocity, or
    /// channel; a silent (velocity 0) or zero-duration note; a
    /// non-power-of-two time signature denominator; a
    /// ticks-per-quarter-note of 0; a tempo that is not finite and
    /// positive, or that does not fit the file format's tempo field;
    /// a gap between events that is too large to encode; or two notes
    /// that share a channel and pitch and overlap in time, since such
    /// a file could not be read back without guessing which note-on
    /// matches which note-off.
    pub fn validate(&self) -> Result<(), MidiError> {
        if let Some((channel, pitch)) = find_overlap(&self.notes) {
            return Err(MidiError::OverlappingNotes { channel, pitch });
        }

        if self.ticks_per_quarter == 0 {
            return Err(MidiError::OutOfRange {
                field: "ticks_per_quarter",
                value: 0,
            });
        }

        if !self.tempo_bpm.is_finite() || self.tempo_bpm <= 0.0 {
            return Err(MidiError::InvalidTempo(self.tempo_bpm));
        }
        let micros_per_quarter = (60_000_000.0 / self.tempo_bpm).round();
        if !(1.0..=16_777_215.0).contains(&micros_per_quarter) {
            return Err(MidiError::TempoOutOfRange);
        }

        if self.time_signature.denominator == 0
            || !self.time_signature.denominator.is_power_of_two()
        {
            return Err(MidiError::OutOfRange {
                field: "time_signature.denominator",
                value: self.time_signature.denominator as u32,
            });
        }

        for note in &self.notes {
            to_u4("channel", note.channel)?;
            to_u7("pitch", note.pitch)?;
            to_u7("velocity", note.velocity)?;
            if note.velocity == 0 {
                return Err(MidiError::SilentNote);
            }
            if note.duration == 0 {
                return Err(MidiError::ZeroDuration);
            }
            note.start
                .checked_add(note.duration)
                .ok_or(MidiError::TickOverflow)?;
        }

        Ok(())
    }

    /// Converts this clip to the bytes of a standard MIDI file, format
    /// 0 (one track). Fails with the same errors as [`Self::validate`].
    pub fn to_smf_bytes(&self) -> Result<Vec<u8>, MidiError> {
        self.validate()?;

        // These recompute values `validate()` already checked, to get
        // the typed values `midly` needs for encoding. `validate()`
        // having passed means none of the `?` calls below can fail.
        let micros_per_quarter = (60_000_000.0 / self.tempo_bpm).round();
        let micros_per_quarter =
            u24::try_from(micros_per_quarter as u32).ok_or(MidiError::TempoOutOfRange)?;
        let denominator_pow2 = self.time_signature.denominator.trailing_zeros() as u8;

        let mut events = Vec::with_capacity(self.notes.len() * 2 + 2);
        events.push(TimedEvent {
            tick: 0,
            order: 0,
            kind: TrackEventKind::Meta(MetaMessage::Tempo(micros_per_quarter)),
        });
        events.push(TimedEvent {
            tick: 0,
            order: 0,
            kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
                self.time_signature.numerator,
                denominator_pow2,
                24,
                8,
            )),
        });

        for note in &self.notes {
            // `validate()` already confirmed these fit; these
            // conversions cannot fail here.
            let channel = to_u4("channel", note.channel)?;
            let pitch = to_u7("pitch", note.pitch)?;
            let velocity = to_u7("velocity", note.velocity)?;
            let end_tick = note
                .start
                .checked_add(note.duration)
                .ok_or(MidiError::TickOverflow)?;

            events.push(TimedEvent {
                tick: note.start,
                order: 2,
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn {
                        key: pitch,
                        vel: velocity,
                    },
                },
            });
            events.push(TimedEvent {
                tick: end_tick,
                order: 1,
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff {
                        key: pitch,
                        vel: u7::from(0),
                    },
                },
            });
        }

        events.sort_by_key(|event| (event.tick, event.order));

        let mut track: Track = Vec::with_capacity(events.len() + 1);
        let mut previous_tick = 0u32;
        for event in events {
            let delta = event.tick.saturating_sub(previous_tick);
            previous_tick = event.tick;
            let delta = u28::try_from(delta).ok_or(MidiError::TickOverflow)?;
            track.push(TrackEvent {
                delta,
                kind: event.kind,
            });
        }
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });

        let smf = Smf {
            header: Header::new(
                Format::SingleTrack,
                Timing::Metrical(u15::try_from(self.ticks_per_quarter).ok_or(
                    MidiError::OutOfRange {
                        field: "ticks_per_quarter",
                        value: self.ticks_per_quarter as u32,
                    },
                )?),
            ),
            tracks: vec![track],
        };

        let mut bytes = Vec::new();
        smf.write(&mut bytes)
            .expect("writing to a Vec<u8> does not fail");
        Ok(bytes)
    }

    /// Reads a clip back from the bytes of a standard MIDI file.
    ///
    /// This function reads every track and merges their notes into one
    /// clip. Each track's delta times count from that track's own
    /// start, which matches the standard MIDI file format. The clip
    /// uses the first tempo and time signature found in any track, and
    /// the defaults from [`MidiClip::default`] if it finds none.
    ///
    /// This function is lenient about the data inside each track,
    /// because it may read files this project did not write:
    ///
    /// - A note-on with no matching note-off is dropped.
    /// - A zero-length note is kept, with its duration rounded up to 1
    ///   tick.
    /// - When two note-on events for the same channel and pitch
    ///   overlap, each note-off is matched to the most recently opened
    ///   note-on (last in, first out). This matches how most
    ///   synthesizers handle a retriggered note.
    /// - A time signature meta event with a denominator too large to
    ///   represent (as a power of two, in a `u8`) is skipped, as if it
    ///   were not there.
    /// - If a track's accumulated tick position would overflow `u32`,
    ///   this function stops reading that one track at that point, and
    ///   keeps the notes already read from it. Other tracks are not
    ///   affected.
    ///
    /// It is strict about the file as a whole, and fails with an
    /// `Err` for: a timing format other than metrical (ticks); a
    /// ticks-per-quarter-note of 0, which cannot express any note
    /// length; and format 2 files (see [`MidiError::UnsupportedFormat`]).
    pub fn from_smf_bytes(bytes: &[u8]) -> Result<MidiClip, MidiError> {
        let smf = Smf::parse(bytes)?;

        if smf.header.format == Format::Sequential {
            return Err(MidiError::UnsupportedFormat);
        }

        let ticks_per_quarter = match smf.header.timing {
            Timing::Metrical(ticks) => u16::from(ticks),
            Timing::Timecode(..) => return Err(MidiError::UnsupportedTiming),
        };
        if ticks_per_quarter == 0 {
            return Err(MidiError::OutOfRange {
                field: "ticks_per_quarter",
                value: 0,
            });
        }

        let mut tempo_bpm = None;
        let mut time_signature = None;
        let mut notes = Vec::new();

        for track in &smf.tracks {
            let mut tick: u32 = 0;
            let mut pending: HashMap<(u8, u8), Vec<(u32, u8)>> = HashMap::new();

            for event in track {
                tick = match tick.checked_add(u32::from(event.delta)) {
                    Some(tick) => tick,
                    // This track's timing has overflowed what this
                    // project can represent. Keep the notes already
                    // read from it, and stop reading further events
                    // from it; any note still open at this point is
                    // dropped, the same as an unmatched note-on.
                    None => break,
                };

                match event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(micros_per_quarter)) => {
                        if tempo_bpm.is_none() {
                            let micros = u32::from(micros_per_quarter).max(1);
                            tempo_bpm = Some(60_000_000.0 / micros as f64);
                        }
                    }
                    TrackEventKind::Meta(MetaMessage::TimeSignature(
                        numerator,
                        denominator_pow2,
                        ..,
                    )) => {
                        // A denominator is `2.pow(denominator_pow2)`. An
                        // exponent of 8 or more does not fit in a `u8`
                        // denominator, so treat it as malformed, and
                        // keep looking for a usable time signature.
                        if time_signature.is_none() && denominator_pow2 < 8 {
                            time_signature = Some(TimeSignature {
                                numerator,
                                denominator: 1u8 << denominator_pow2,
                            });
                        }
                    }
                    TrackEventKind::Midi { channel, message } => {
                        let channel = u8::from(channel);
                        match message {
                            MidiMessage::NoteOn { key, vel } if u8::from(vel) > 0 => {
                                pending
                                    .entry((channel, u8::from(key)))
                                    .or_default()
                                    .push((tick, u8::from(vel)));
                            }
                            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                                let map_key = (channel, u8::from(key));
                                if let Some(stack) = pending.get_mut(&map_key) {
                                    if let Some((start, velocity)) = stack.pop() {
                                        notes.push(Note {
                                            pitch: u8::from(key),
                                            velocity,
                                            start,
                                            duration: tick.saturating_sub(start).max(1),
                                            channel,
                                        });
                                    }
                                    if stack.is_empty() {
                                        pending.remove(&map_key);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        notes.sort_by_key(|note| (note.start, note.pitch, note.channel));

        Ok(MidiClip {
            ticks_per_quarter,
            tempo_bpm: tempo_bpm.unwrap_or(120.0),
            time_signature: time_signature.unwrap_or_default(),
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_clip() -> MidiClip {
        MidiClip {
            ticks_per_quarter: 480,
            tempo_bpm: 140.0,
            time_signature: TimeSignature {
                numerator: 3,
                denominator: 4,
            },
            notes: vec![
                Note {
                    pitch: 60,
                    velocity: 100,
                    start: 0,
                    duration: 480,
                    channel: 0,
                },
                Note {
                    pitch: 64,
                    velocity: 90,
                    start: 480,
                    duration: 240,
                    channel: 0,
                },
                Note {
                    pitch: 67,
                    velocity: 80,
                    start: 480,
                    duration: 480,
                    channel: 1,
                },
            ],
        }
    }

    fn note(pitch: u8, velocity: u8, start: u32, duration: u32, channel: u8) -> Note {
        Note {
            pitch,
            velocity,
            start,
            duration,
            channel,
        }
    }

    #[test]
    fn round_trip_keeps_notes() {
        let clip = sample_clip();
        let bytes = clip.to_smf_bytes().expect("clip should encode");
        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");

        assert_eq!(parsed.ticks_per_quarter, clip.ticks_per_quarter);
        assert_eq!(parsed.notes, clip.notes);
    }

    #[test]
    fn round_trip_keeps_tempo_and_time_signature() {
        let clip = sample_clip();
        let bytes = clip.to_smf_bytes().expect("clip should encode");
        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");

        assert!((parsed.tempo_bpm - clip.tempo_bpm).abs() < 0.01);
        assert_eq!(parsed.time_signature, clip.time_signature);
    }

    #[test]
    fn empty_clip_round_trips() {
        let clip = MidiClip::default();
        let bytes = clip.to_smf_bytes().expect("empty clip should encode");
        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");

        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn out_of_range_pitch_is_rejected() {
        let clip = MidiClip {
            notes: vec![note(200, 100, 0, 480, 0)],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("pitch 200 should be rejected");
        assert!(matches!(err, MidiError::OutOfRange { field: "pitch", .. }));
    }

    #[test]
    fn validate_rejects_what_to_smf_bytes_rejects_without_encoding() {
        let bad_clip = MidiClip {
            notes: vec![note(200, 100, 0, 480, 0)],
            ..MidiClip::default()
        };
        assert!(matches!(
            bad_clip.validate(),
            Err(MidiError::OutOfRange { field: "pitch", .. })
        ));

        let good_clip = sample_clip();
        assert!(good_clip.validate().is_ok());
    }

    #[test]
    fn zero_velocity_note_is_rejected() {
        let clip = MidiClip {
            notes: vec![note(60, 0, 0, 480, 0)],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("velocity 0 should be rejected");
        assert!(matches!(err, MidiError::SilentNote));
    }

    #[test]
    fn zero_duration_note_is_rejected() {
        let clip = MidiClip {
            notes: vec![note(60, 100, 0, 0, 0)],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("duration 0 should be rejected");
        assert!(matches!(err, MidiError::ZeroDuration));
    }

    #[test]
    fn overlapping_same_pitch_and_channel_is_rejected_on_export() {
        let clip = MidiClip {
            notes: vec![note(60, 100, 0, 100, 0), note(60, 90, 50, 100, 0)],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("overlapping notes should be rejected");
        assert!(matches!(
            err,
            MidiError::OverlappingNotes {
                channel: 0,
                pitch: 60
            }
        ));
    }

    #[test]
    fn back_to_back_same_pitch_notes_are_allowed() {
        // The first note ends exactly when the second one starts. This
        // is not an overlap.
        let clip = MidiClip {
            notes: vec![note(60, 100, 0, 100, 0), note(60, 90, 100, 100, 0)],
            ..MidiClip::default()
        };

        let bytes = clip
            .to_smf_bytes()
            .expect("back-to-back notes should encode");
        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");
        assert_eq!(parsed.notes, clip.notes);
    }

    #[test]
    fn invalid_time_signature_denominator_is_rejected() {
        let clip = MidiClip {
            time_signature: TimeSignature {
                numerator: 4,
                denominator: 3,
            },
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("denominator 3 is not a power of two");
        assert!(matches!(
            err,
            MidiError::OutOfRange {
                field: "time_signature.denominator",
                ..
            }
        ));
    }

    #[test]
    fn non_finite_tempo_is_rejected() {
        let clip = MidiClip {
            tempo_bpm: f64::NAN,
            ..MidiClip::default()
        };
        assert!(matches!(
            clip.to_smf_bytes(),
            Err(MidiError::InvalidTempo(_))
        ));

        let clip = MidiClip {
            tempo_bpm: -10.0,
            ..MidiClip::default()
        };
        assert!(matches!(
            clip.to_smf_bytes(),
            Err(MidiError::InvalidTempo(_))
        ));
    }

    #[test]
    fn tempo_too_slow_to_encode_is_rejected() {
        // At 1 beat per minute, one quarter note takes 60,000,000
        // microseconds, which does not fit in the file format's 24-bit
        // tempo field (max 16,777,215). This is different from
        // `non_finite_tempo_is_rejected`: the value here is finite and
        // positive, just outside what the file format can store.
        let clip = MidiClip {
            tempo_bpm: 1.0,
            ..MidiClip::default()
        };
        assert!(matches!(
            clip.to_smf_bytes(),
            Err(MidiError::TempoOutOfRange)
        ));
    }

    #[test]
    fn malformed_bytes_do_not_panic() {
        let err = MidiClip::from_smf_bytes(b"not a midi file")
            .expect_err("garbage bytes should not parse");
        assert!(matches!(err, MidiError::Parse(_)));
    }

    #[test]
    fn large_gap_is_rejected_on_export() {
        // u28's maximum value is 268,435,455. A gap larger than that
        // cannot be stored as one delta time.
        let clip = MidiClip {
            notes: vec![note(60, 100, 0, 300_000_000, 0)],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("a gap this large should be rejected");
        assert!(matches!(err, MidiError::TickOverflow));
    }

    #[test]
    fn to_smf_bytes_is_deterministic() {
        let clip = sample_clip();
        let first = clip.to_smf_bytes().expect("clip should encode");
        let second = clip.to_smf_bytes().expect("clip should encode again");
        assert_eq!(first, second);
    }

    /// Builds the bytes of a single-track standard MIDI file directly
    /// with `midly` types, so the test can create event sequences that
    /// `MidiClip::to_smf_bytes` would reject, such as overlapping notes.
    fn build_single_track_smf(ticks_per_quarter: u16, track: Track<'static>) -> Vec<u8> {
        let smf = Smf {
            header: Header::new(
                Format::SingleTrack,
                Timing::Metrical(u15::try_from(ticks_per_quarter).unwrap()),
            ),
            tracks: vec![track],
        };
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn overlapping_notes_use_last_in_first_out_matching_on_import() {
        // Note A: pitch 60, starts at tick 0.
        // Note B: pitch 60, same channel, starts at tick 50, while A is
        // still playing.
        // Event order: NoteOn A @0, NoteOn B @50, NoteOff @100, NoteOff @150.
        // LIFO matching pairs the first NoteOff with B (the most
        // recently opened note), and the second NoteOff with A.
        let track: Track = vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(100),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(50),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(90),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(50),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(0),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(50),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(0),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ];
        let bytes = build_single_track_smf(480, track);

        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");
        assert_eq!(
            parsed.notes,
            vec![note(60, 100, 0, 150, 0), note(60, 90, 50, 50, 0)]
        );
    }

    #[test]
    fn multi_track_import_merges_notes_from_every_track() {
        let track_a: Track = vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(100),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(100),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(0),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ];
        let track_b: Track = vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Midi {
                    channel: u4::from(1),
                    message: MidiMessage::NoteOn {
                        key: u7::from(64),
                        vel: u7::from(90),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(200),
                kind: TrackEventKind::Midi {
                    channel: u4::from(1),
                    message: MidiMessage::NoteOff {
                        key: u7::from(64),
                        vel: u7::from(0),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ];

        let smf = Smf {
            header: Header::new(
                Format::Parallel,
                Timing::Metrical(u15::try_from(480).unwrap()),
            ),
            tracks: vec![track_a, track_b],
        };
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();

        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");
        assert_eq!(
            parsed.notes,
            vec![note(60, 100, 0, 100, 0), note(64, 90, 0, 200, 1)]
        );
    }

    #[test]
    fn zero_ticks_per_quarter_is_rejected_on_export() {
        let clip = MidiClip {
            ticks_per_quarter: 0,
            ..MidiClip::default()
        };
        let err = clip
            .to_smf_bytes()
            .expect_err("ticks_per_quarter 0 should be rejected");
        assert!(matches!(
            err,
            MidiError::OutOfRange {
                field: "ticks_per_quarter",
                value: 0
            }
        ));
    }

    #[test]
    fn zero_ticks_per_quarter_is_rejected_on_import() {
        let track: Track = vec![TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        }];
        let bytes = build_single_track_smf(0, track);

        let err =
            MidiClip::from_smf_bytes(&bytes).expect_err("ticks_per_quarter 0 should be rejected");
        assert!(matches!(
            err,
            MidiError::OutOfRange {
                field: "ticks_per_quarter",
                value: 0
            }
        ));
    }

    #[test]
    fn malformed_time_signature_denominator_is_skipped_on_import() {
        // A denominator exponent of 8 or more does not fit in a `u8`
        // denominator (2.pow(8) = 256). This must not be treated as a
        // valid time signature.
        let track: Track = vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::TimeSignature(4, 250, 24, 8)),
            },
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ];
        let bytes = build_single_track_smf(480, track);

        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");
        assert_eq!(parsed.time_signature, TimeSignature::default());
    }

    #[test]
    fn format_2_sequential_files_are_rejected() {
        let track: Track = vec![TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        }];
        let smf = Smf {
            header: Header::new(
                Format::Sequential,
                Timing::Metrical(u15::try_from(480).unwrap()),
            ),
            tracks: vec![track.clone(), track],
        };
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();

        let err = MidiClip::from_smf_bytes(&bytes).expect_err("format 2 should be rejected");
        assert!(matches!(err, MidiError::UnsupportedFormat));
    }

    #[test]
    fn tick_overflow_truncates_only_the_affected_track() {
        // u28::MAX is 268,435,455. Accumulating it 16 times, starting
        // from tick 100, overflows u32::MAX (4,294,967,295) on the
        // 16th addition. Build a track with one complete note before
        // that point, 15 harmless filler events that do not overflow,
        // and then a 16th filler event that does. A note placed after
        // the 16th filler event must never be reached.
        let mut track: Track = vec![
            TrackEvent {
                delta: u28::from(0),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(100),
                    },
                },
            },
            TrackEvent {
                delta: u28::from(100),
                kind: TrackEventKind::Midi {
                    channel: u4::from(0),
                    message: MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(0),
                    },
                },
            },
        ];
        for _ in 0..16 {
            track.push(TrackEvent {
                delta: u28::from(268_435_455),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(500_000))),
            });
        }
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Midi {
                channel: u4::from(0),
                message: MidiMessage::NoteOn {
                    key: u7::from(72),
                    vel: u7::from(90),
                },
            },
        });
        track.push(TrackEvent {
            delta: u28::from(10),
            kind: TrackEventKind::Midi {
                channel: u4::from(0),
                message: MidiMessage::NoteOff {
                    key: u7::from(72),
                    vel: u7::from(0),
                },
            },
        });
        track.push(TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });

        let bytes = build_single_track_smf(480, track);
        let parsed = MidiClip::from_smf_bytes(&bytes).expect("bytes should decode");

        // Only the note fully read before the overflow point survives.
        assert_eq!(parsed.notes, vec![note(60, 100, 0, 100, 0)]);
    }
}
