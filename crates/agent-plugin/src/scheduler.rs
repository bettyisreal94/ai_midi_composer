//! Pure, host-independent MIDI scheduling logic.
//!
//! This module has no `nih_plug` types in it. It only computes *what*
//! MIDI events a looping clip should produce for a given stretch of
//! samples. `lib.rs` is responsible for calling `context.send_event()`
//! with the results. Keeping the two separated means this module can
//! have plain `#[test]` unit tests, with no host or audio callback
//! needed.
//!
//! A review of Phase 2 (see `REVIEW.md`) found three real bugs in the
//! first version of this scheduler, all fixed here:
//!
//! 1. A note ending exactly on the loop boundary never got its
//!    note-off, because the half-open interval check excluded that
//!    exact tick from both the chunk that ends there and the chunk
//!    that starts there. Fixed by using an asymmetric interval: a note
//!    start uses `[local_tick, tick_end)`, and a note end uses
//!    `(local_tick, tick_end]`. See `loop_boundary_note_off_is_not_lost`.
//! 2. Stopping the transport, or a seek, could leave a note stuck on
//!    forever, because nothing tracked which notes were currently
//!    sounding, or sent cleanup note-offs. Fixed with [`ActiveNotes`]
//!    and [`stop_all_notes`], called from `lib.rs` on stop and on a
//!    detected transport discontinuity.
//! 3. Note-off and note-on events at the same sample were sent in
//!    whatever order the clip's notes happened to be stored in, not a
//!    defined order. Fixed by sorting scheduled events so a note-off
//!    always sorts before a note-on at the same sample.

use agent_core::midi::MidiClip;

/// Tracks which `(channel, pitch)` pairs currently have a note-on that
/// this plugin has sent, with no matching note-off sent yet. Used to
/// send cleanup note-offs when playback stops, or jumps, so a note is
/// never left stuck on at the host.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActiveNotes {
    notes: Vec<(u8, u8)>,
}

impl ActiveNotes {
    fn mark_on(&mut self, channel: u8, pitch: u8) {
        if !self.notes.contains(&(channel, pitch)) {
            self.notes.push((channel, pitch));
        }
    }

    fn mark_off(&mut self, channel: u8, pitch: u8) {
        self.notes.retain(|&(c, p)| (c, p) != (channel, pitch));
    }

    /// Returns true if the given channel and pitch currently has an
    /// unmatched note-on. Exposed for tests.
    #[cfg(test)]
    fn is_active(&self, channel: u8, pitch: u8) -> bool {
        self.notes.contains(&(channel, pitch))
    }

    /// The number of notes with an unmatched note-on. Exposed for
    /// tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.notes.len()
    }
}

/// One MIDI event this plugin should send, at a sample offset within
/// the current process block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScheduledEvent {
    /// The sample offset within the current process block.
    pub timing: u32,
    pub channel: u8,
    pub pitch: u8,
    pub kind: ScheduledEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScheduledEventKind {
    NoteOn { velocity: f32 },
    NoteOff,
}

/// Builds cleanup note-off events for every currently active note, and
/// clears `active`. Call this when playback stops, or when a
/// discontinuity (seek, loop, punch-in) is detected, so no note is
/// left stuck on. The returned events all use `timing: 0`, since they
/// must play before anything else in the block they are added to.
pub fn stop_all_notes(active: &mut ActiveNotes) -> Vec<ScheduledEvent> {
    std::mem::take(&mut active.notes)
        .into_iter()
        .map(|(channel, pitch)| ScheduledEvent {
            timing: 0,
            channel,
            pitch,
            kind: ScheduledEventKind::NoteOff,
        })
        .collect()
}

/// Computes the events `clip` produces while its playhead moves from
/// `start_tick` for `num_samples` samples, at a rate of
/// `samples_per_tick` samples per tick, looping every
/// `loop_length_ticks` ticks. Updates `active` to match. Returns the
/// events, in the order they must be sent, and the new playhead
/// position (wrapped to `0..loop_length_ticks`).
///
/// `start_tick` must already be wrapped to `0..loop_length_ticks`.
/// This function has no side effects: it does not touch a host, a
/// clock, or `active` beyond the `&mut` passed in, so it is safe to
/// call from a plain unit test.
pub fn schedule_clip_events(
    clip: &MidiClip,
    loop_length_ticks: u32,
    start_tick: f64,
    samples_per_tick: f64,
    num_samples: usize,
    active: &mut ActiveNotes,
) -> (Vec<ScheduledEvent>, f64) {
    let loop_len_ticks = loop_length_ticks as f64;
    let mut events = Vec::new();
    let mut local_tick = start_tick;
    let mut block_offset = 0u32;
    let mut remaining = num_samples;

    while remaining > 0 {
        let ticks_left_in_loop = (loop_len_ticks - local_tick).max(0.0);
        let samples_left_in_loop = (ticks_left_in_loop * samples_per_tick).ceil() as usize;
        let chunk = remaining.min(samples_left_in_loop.max(1));
        let tick_end = local_tick + chunk as f64 / samples_per_tick;

        for note in &clip.notes {
            let start = note.start as f64;
            let end = start + note.duration as f64;

            // Note starts use a half-open interval, `[local_tick,
            // tick_end)`: a start exactly at the beginning of this
            // chunk fires now; a start exactly at the end belongs to
            // the next chunk, so it is not fired twice.
            if start >= local_tick && start < tick_end {
                let timing =
                    block_offset + ((start - local_tick) * samples_per_tick).round() as u32;
                events.push(ScheduledEvent {
                    timing: timing.min(num_samples.saturating_sub(1) as u32),
                    channel: note.channel,
                    pitch: note.pitch,
                    kind: ScheduledEventKind::NoteOn {
                        velocity: note.velocity as f32 / 127.0,
                    },
                });
                active.mark_on(note.channel, note.pitch);
            }

            // Note ends use the opposite convention, `(local_tick,
            // tick_end]`: an end exactly at the boundary between two
            // chunks belongs to the chunk it closes out, not the next
            // one. Without this, a note ending exactly on the loop
            // boundary is never closed (see `loop_boundary_note_off_is_not_lost`).
            if end > local_tick && end <= tick_end {
                let timing = block_offset + ((end - local_tick) * samples_per_tick).round() as u32;
                events.push(ScheduledEvent {
                    timing: timing.min(num_samples.saturating_sub(1) as u32),
                    channel: note.channel,
                    pitch: note.pitch,
                    kind: ScheduledEventKind::NoteOff,
                });
                active.mark_off(note.channel, note.pitch);
            }
        }

        remaining -= chunk;
        block_offset += chunk as u32;
        local_tick += chunk as f64 / samples_per_tick;
        if local_tick >= loop_len_ticks {
            local_tick -= loop_len_ticks;
        }
    }

    // A note-off must sort before a note-on at the same sample,
    // regardless of the order the clip's notes happen to be stored
    // in. This matches the file writer's convention in `agent-core`.
    events.sort_by_key(|event| {
        let priority = match event.kind {
            ScheduledEventKind::NoteOff => 0,
            ScheduledEventKind::NoteOn { .. } => 1,
        };
        (event.timing, priority)
    });

    (events, local_tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::midi::{Note, TimeSignature};

    /// A one-octave, four note arpeggio, one beat apart, looping every
    /// 4 beats. The same shape as `demo_clip()` in `lib.rs`, kept as a
    /// separate copy here so this module's tests do not depend on
    /// `lib.rs`.
    fn arpeggio(ticks_per_quarter: u32) -> (MidiClip, u32) {
        let beat = ticks_per_quarter;
        let clip = MidiClip {
            ticks_per_quarter: ticks_per_quarter as u16,
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
        };
        (clip, beat * 4)
    }

    #[test]
    fn single_note_gets_on_and_off() {
        let clip = MidiClip {
            ticks_per_quarter: 960,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: vec![Note {
                pitch: 60,
                velocity: 100,
                start: 0,
                duration: 960,
                channel: 0,
            }],
        };
        let mut active = ActiveNotes::default();
        // 1 sample per tick, so the whole 4-beat loop (3840 ticks)
        // needs 3840 samples. Ask for exactly the note's own length.
        let (events, _) = schedule_clip_events(&clip, 3840, 0.0, 1.0, 960, &mut active);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].timing, 0);
        assert!(matches!(events[0].kind, ScheduledEventKind::NoteOn { .. }));
        assert_eq!(events[1].timing, 959);
        assert!(matches!(events[1].kind, ScheduledEventKind::NoteOff));
        // The note-off already closed it, in the same call.
        assert!(!active.is_active(0, 60));
    }

    #[test]
    fn note_left_open_across_calls_stays_active() {
        let clip = MidiClip {
            ticks_per_quarter: 960,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: vec![Note {
                pitch: 60,
                velocity: 100,
                start: 0,
                duration: 960,
                channel: 0,
            }],
        };
        let mut active = ActiveNotes::default();
        // Only ask for the first half of the note. Its note-on should
        // fire, but not its note-off yet.
        let (events, next_tick) = schedule_clip_events(&clip, 3840, 0.0, 1.0, 480, &mut active);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].kind, ScheduledEventKind::NoteOn { .. }));
        assert!(active.is_active(0, 60));
        assert_eq!(next_tick, 480.0);
    }

    #[test]
    fn loop_boundary_note_off_is_not_lost() {
        // This is the regression test for the bug found in the
        // review: the arpeggio's last note ends exactly on the loop
        // boundary. Ask for two full loops in one call, which forces
        // the scheduler to wrap around mid-call, and check every note
        // gets both its on and its off, in both loops.
        let (clip, loop_len) = arpeggio(960);
        let mut active = ActiveNotes::default();
        let num_samples = (loop_len * 2) as usize;
        let (events, next_tick) =
            schedule_clip_events(&clip, loop_len, 0.0, 1.0, num_samples, &mut active);

        let on_count = events
            .iter()
            .filter(|e| matches!(e.kind, ScheduledEventKind::NoteOn { .. }))
            .count();
        let off_count = events
            .iter()
            .filter(|e| matches!(e.kind, ScheduledEventKind::NoteOff))
            .count();
        assert_eq!(on_count, 8, "4 notes x 2 loops");
        assert_eq!(off_count, 8, "every note-on must have a matching note-off");
        assert_eq!(active.len(), 0, "no note should still be active");
        assert_eq!(next_tick, 0.0, "two full loops should land back at tick 0");
    }

    #[test]
    fn starting_in_the_middle_of_a_loop_only_plays_remaining_notes() {
        let (clip, loop_len) = arpeggio(960);
        let mut active = ActiveNotes::default();
        // Start exactly on the third note (tick 1920), and ask for
        // just enough samples to reach the loop boundary.
        let (events, next_tick) =
            schedule_clip_events(&clip, loop_len, 1920.0, 1.0, 1920, &mut active);

        let on_pitches: Vec<u8> = events
            .iter()
            .filter_map(|e| match e.kind {
                ScheduledEventKind::NoteOn { .. } => Some(e.pitch),
                ScheduledEventKind::NoteOff => None,
            })
            .collect();
        assert_eq!(on_pitches, vec![67, 72]);
        assert_eq!(next_tick, 0.0);
        assert_eq!(active.len(), 0);
    }

    #[test]
    fn note_off_sorts_before_note_on_at_the_same_sample() {
        // Two notes, back to back: the first ends exactly when the
        // second starts. Store them in the clip in a deliberately
        // unhelpful order (the later note first), to prove the sort
        // does not depend on the clip's own note order.
        let clip = MidiClip {
            ticks_per_quarter: 960,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: vec![
                Note {
                    pitch: 64,
                    velocity: 100,
                    start: 960,
                    duration: 960,
                    channel: 0,
                },
                Note {
                    pitch: 60,
                    velocity: 100,
                    start: 0,
                    duration: 960,
                    channel: 0,
                },
            ],
        };
        let mut active = ActiveNotes::default();
        let (events, _) = schedule_clip_events(&clip, 1920, 0.0, 1.0, 1920, &mut active);

        let at_boundary: Vec<&ScheduledEvent> = events.iter().filter(|e| e.timing == 960).collect();
        assert_eq!(at_boundary.len(), 2);
        assert!(matches!(at_boundary[0].kind, ScheduledEventKind::NoteOff));
        assert!(matches!(
            at_boundary[1].kind,
            ScheduledEventKind::NoteOn { .. }
        ));
    }

    #[test]
    fn stop_all_notes_closes_every_active_note_and_clears_the_set() {
        let mut active = ActiveNotes::default();
        active.mark_on(0, 60);
        active.mark_on(1, 67);

        let events = stop_all_notes(&mut active);

        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|e| e.timing == 0 && matches!(e.kind, ScheduledEventKind::NoteOff)));
        assert_eq!(active.len(), 0);
    }

    #[test]
    fn stop_all_notes_on_empty_set_is_a_no_op() {
        let mut active = ActiveNotes::default();
        assert!(stop_all_notes(&mut active).is_empty());
    }
}
