//! The MIDI data model.
//!
//! A [`MidiClip`] holds a list of [`Note`] values, plus tempo and time
//! signature. This module also converts a [`MidiClip`] to the bytes of a
//! standard MIDI file, and back.

use std::collections::HashMap;
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
    /// 0 to 127.
    pub velocity: u8,
    /// The start time, in ticks from the start of the clip.
    pub start: u32,
    /// The note length, in ticks. Must be greater than 0.
    pub duration: u32,
    /// The MIDI channel. Valid range: 0 to 15.
    pub channel: u8,
}

/// A musical time signature, such as 4/4 or 3/4.
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
    /// The playback tempo, in beats per minute.
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
    /// A note field does not fit the MIDI file format, such as a pitch
    /// above 127.
    OutOfRange { field: &'static str, value: u32 },
    /// The MIDI file bytes could not be parsed.
    Parse(midly::Error),
    /// The file uses a timing format this project does not support.
    UnsupportedTiming,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::OutOfRange { field, value } => {
                write!(f, "value {value} does not fit in field '{field}'")
            }
            MidiError::Parse(err) => write!(f, "could not parse the MIDI file: {err}"),
            MidiError::UnsupportedTiming => {
                write!(f, "this project only supports metrical (ticks) timing")
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
    /// Converts this clip to the bytes of a standard MIDI file, format
    /// 0 (one track).
    pub fn to_smf_bytes(&self) -> Result<Vec<u8>, MidiError> {
        let mut events = Vec::with_capacity(self.notes.len() * 2 + 2);

        let micros_per_quarter = if self.tempo_bpm > 0.0 {
            (60_000_000.0 / self.tempo_bpm).round() as u32
        } else {
            500_000
        };
        events.push(TimedEvent {
            tick: 0,
            order: 0,
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(micros_per_quarter))),
        });

        let denominator_pow2 = self.time_signature.denominator.trailing_zeros() as u8;
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
            let channel = to_u4("channel", note.channel)?;
            let pitch = to_u7("pitch", note.pitch)?;
            let velocity = to_u7("velocity", note.velocity)?;
            let end_tick = note.start.saturating_add(note.duration.max(1));

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
            track.push(TrackEvent {
                delta: u28::from(delta),
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
    /// clip. It uses the first tempo and time signature it finds, and
    /// the defaults from [`MidiClip::default`] if it finds none. It
    /// silently drops any note-on event that has no matching note-off.
    pub fn from_smf_bytes(bytes: &[u8]) -> Result<MidiClip, MidiError> {
        let smf = Smf::parse(bytes)?;

        let ticks_per_quarter = match smf.header.timing {
            Timing::Metrical(ticks) => u16::from(ticks),
            Timing::Timecode(..) => return Err(MidiError::UnsupportedTiming),
        };

        let mut tempo_bpm = None;
        let mut time_signature = None;
        let mut notes = Vec::new();

        for track in &smf.tracks {
            let mut tick: u32 = 0;
            let mut pending: HashMap<(u8, u8), (u32, u8)> = HashMap::new();

            for event in track {
                tick = tick.saturating_add(u32::from(event.delta));

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
                        if time_signature.is_none() {
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
                                pending.insert((channel, u8::from(key)), (tick, u8::from(vel)));
                            }
                            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                                if let Some((start, velocity)) =
                                    pending.remove(&(channel, u8::from(key)))
                                {
                                    notes.push(Note {
                                        pitch: u8::from(key),
                                        velocity,
                                        start,
                                        duration: tick.saturating_sub(start).max(1),
                                        channel,
                                    });
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
            notes: vec![Note {
                pitch: 200,
                velocity: 100,
                start: 0,
                duration: 480,
                channel: 0,
            }],
            ..MidiClip::default()
        };

        let err = clip
            .to_smf_bytes()
            .expect_err("pitch 200 should be rejected");
        assert!(matches!(err, MidiError::OutOfRange { field: "pitch", .. }));
    }
}
