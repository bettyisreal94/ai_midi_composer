//! Pure, host-independent MIDI scheduling logic.
//!
//! This module has no `nih_plug` types in it. It only computes *what*
//! MIDI events a looping clip should produce for a given stretch of
//! samples. `lib.rs` is responsible for actually sending them to the
//! host. Keeping the two separated means this module can have plain
//! `#[test]` unit tests, with no host or audio callback needed.
//!
//! A review after Phase 2 (see `REVIEW.md`) found three real bugs in
//! the first version of this scheduler, fixed then:
//!
//! 1. A note ending exactly on the loop boundary never got its
//!    note-off. Fixed with an asymmetric interval: a note start uses
//!    `[local_tick, tick_end)`, and a note end uses
//!    `(local_tick, tick_end]`.
//! 2. Stopping the transport, or a seek, could leave a note stuck on
//!    forever. Fixed with [`ActiveNotes`] and [`stop_all_notes`].
//! 3. Note-off and note-on events at the same sample were sent in
//!    whatever order the clip's notes happened to be stored in. Fixed
//!    by sorting scheduled events so a note-off always sorts before a
//!    note-on at the same sample.
//!
//! A review after Phase 6 found that this fix was incomplete: despite
//! `TODO.md`'s claim that the live path was real-time safe, this
//! module still allocated a fresh `Vec<ScheduledEvent>` on every
//! `process()` call, sorted it every time, and stored active notes in
//! a growable `Vec`, all on the audio thread. This version removes
//! every one of those:
//!
//! - [`PlayableClip`] converts a clip's notes into one sorted, flat
//!   list of on/off events *once*, when the clip is published (see
//!   `clip_publisher.rs`), not on every audio callback.
//! - [`schedule_events`] walks that list with a cursor the caller
//!   keeps between calls, instead of scanning every note and rebuilding
//!   a sorted list every block. A cursor only needs to jump backward or
//!   forward by binary search after a discontinuity; see
//!   [`PlayableClip::index_at_or_after`].
//! - Both [`schedule_events`] and [`stop_all_notes`] call an
//!   `emit: impl FnMut(ScheduledEvent)` callback directly, instead of
//!   returning a `Vec`. `lib.rs` passes a closure that calls
//!   `context.send_event()`, so no event is ever buffered in a
//!   heap-allocated list on the audio thread. Tests can still collect
//!   emitted events into a plain `Vec` for assertions, since a test is
//!   not running on an audio thread.
//! - [`ActiveNotes`] is a fixed `[[u8; 128]; 16]` grid (16 channels by
//!   128 pitches, MIDI's own limits), not a growable `Vec`. Counting,
//!   rather than a single flag per key, also means it can tell how
//!   many overlapping note-ons are active for the same channel and
//!   pitch, which the earlier `Vec`-based version could not.
//!
//! A later review found two more real bugs in this rewrite itself,
//! both reported by a maintainer who saw the plugin's memory use grow
//! without bound (past 50 GB) as soon as it started, and confirmed by
//! running this module's own tests, which hung instead of finishing:
//!
//! 1. [`schedule_events`]'s inner loop wrapped `*cursor` back to 0
//!    whenever it reached the end of the event list, with no limit on
//!    how many times it could do that inside one call. Whenever a
//!    call's window spanned exactly one full loop (which happens on
//!    the very first `process()` call, since playback starts at tick
//!    0), the wrapped cursor re-checked the same window against events
//!    it had just emitted moments earlier, which still passed the same
//!    test that admitted them the first time, so it emitted them
//!    again, wrapped again, and never stopped: an infinite loop that
//!    handed the host an unbounded stream of note events from the
//!    audio thread. A call can only ever touch each event in the list
//!    once (its window never spans more than one full loop), so a
//!    plain counter, capped at the event count, closes this with no
//!    allocation.
//! 2. [`PlayableClip::index_at_or_after`] compared only an event's tick
//!    against the target tick, with no regard for the event's kind. It
//!    could return a note-off sitting exactly at the target tick, which
//!    [`schedule_events`]'s own window test for a note-off
//!    (`event_tick > local_tick`, deliberately strict; see point 1
//!    above the earlier list) always rejects when resuming exactly at
//!    that tick. That made the very first window check after a
//!    discontinuity fail immediately, before ever reaching the
//!    note-on that starts at the same tick, right after it in the
//!    sorted list, so nothing further got scheduled at all. Skipping a
//!    same-tick note-off, while still landing on a same-tick note-on,
//!    fixes this, and matches [`schedule_events`]'s own two window
//!    tests exactly.

use composer_core::midi::MidiClip;

/// Tracks how many note-on events this plugin has sent for each
/// `(channel, pitch)` pair, with no matching note-off sent yet. Used
/// to send cleanup note-offs when playback stops, or jumps, so a note
/// is never left stuck on at the host.
///
/// A fixed-size grid, not a `Vec`: creating, growing, or dropping one
/// never touches the allocator, so it is safe to hold on the audio
/// thread. Counts, rather than one flag per key, because two
/// overlapping notes at the same channel and pitch are possible (see
/// `composer_core::midi`'s LIFO import policy), and both need their own
/// note-off sent on cleanup.
pub struct ActiveNotes {
    counts: [[u8; 128]; 16],
}

impl Default for ActiveNotes {
    fn default() -> Self {
        Self {
            counts: [[0; 128]; 16],
        }
    }
}

impl ActiveNotes {
    fn mark_on(&mut self, channel: u8, pitch: u8) {
        let count = &mut self.counts[channel as usize][pitch as usize];
        *count = count.saturating_add(1);
    }

    fn mark_off(&mut self, channel: u8, pitch: u8) {
        let count = &mut self.counts[channel as usize][pitch as usize];
        *count = count.saturating_sub(1);
    }

    /// Returns true if the given channel and pitch currently has at
    /// least one unmatched note-on. Exposed for tests.
    #[cfg(test)]
    fn is_active(&self, channel: u8, pitch: u8) -> bool {
        self.counts[channel as usize][pitch as usize] > 0
    }

    /// How many distinct `(channel, pitch)` pairs have at least one
    /// unmatched note-on. Exposed for tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.counts
            .iter()
            .flatten()
            .filter(|&&count| count > 0)
            .count()
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

/// Calls `emit` once for every currently active note, as a note-off,
/// and clears `active`. Call this when playback stops, or when a
/// discontinuity (seek, loop, punch-in, clip replacement) is detected,
/// so no note is left stuck on. Every event uses `timing: 0`, since it
/// must play before anything else in the block it is added to.
pub fn stop_all_notes(active: &mut ActiveNotes, emit: &mut impl FnMut(ScheduledEvent)) {
    for channel in 0u8..16 {
        for pitch in 0u8..128 {
            if active.counts[channel as usize][pitch as usize] > 0 {
                active.counts[channel as usize][pitch as usize] = 0;
                emit(ScheduledEvent {
                    timing: 0,
                    channel,
                    pitch,
                    kind: ScheduledEventKind::NoteOff,
                });
            }
        }
    }
}

/// One entry in a [`PlayableClip`]'s precomputed event list.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlayableEvent {
    tick: u32,
    channel: u8,
    pitch: u8,
    kind: ScheduledEventKind,
}

/// A clip's notes, converted into one flat, time-sorted list of on/off
/// events, and the loop length that goes with them. Building this list
/// involves a heap allocation and a sort, so it is done once, when a
/// clip is published (see `clip_publisher.rs`), never on the audio
/// thread.
#[derive(Clone)]
pub struct PlayableClip {
    events: Vec<PlayableEvent>,
    pub loop_length_ticks: u32,
}

impl PlayableClip {
    /// Builds a `PlayableClip` from `clip`. Not real-time safe: this
    /// allocates and sorts. Call it from the editor or the background
    /// task executor, when a clip is published, not from `process()`.
    pub fn from_clip(clip: &MidiClip) -> Self {
        let mut events = Vec::with_capacity(clip.notes.len() * 2);
        for note in &clip.notes {
            events.push(PlayableEvent {
                tick: note.start,
                channel: note.channel,
                pitch: note.pitch,
                kind: ScheduledEventKind::NoteOn {
                    velocity: note.velocity as f32 / 127.0,
                },
            });
            events.push(PlayableEvent {
                tick: note.start.saturating_add(note.duration),
                channel: note.channel,
                pitch: note.pitch,
                kind: ScheduledEventKind::NoteOff,
            });
        }
        // A note-off must sort before a note-on at the same tick,
        // regardless of the order the clip's notes happen to be
        // stored in. This matches the file writer's convention in
        // `composer-core`.
        events.sort_by_key(|event| {
            let priority = match event.kind {
                ScheduledEventKind::NoteOff => 0,
                ScheduledEventKind::NoteOn { .. } => 1,
            };
            (event.tick, priority)
        });

        let loop_length_ticks = clip
            .notes
            .iter()
            .map(|note| note.start.saturating_add(note.duration))
            .max()
            .unwrap_or(clip.ticks_per_quarter as u32 * 4);

        Self {
            events,
            loop_length_ticks,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The index of the first event that [`schedule_events`] would
    /// actually accept when resuming playback exactly at `tick`, found
    /// by binary search, not a scan. Used to relocate the cursor
    /// [`schedule_events`] needs after a discontinuity, without
    /// walking from the start of the list.
    ///
    /// A review found that comparing only `event.tick` against `tick`,
    /// with no regard for the event's kind, could return a note-off
    /// sitting exactly at `tick`. `schedule_events`'s own window test
    /// for a note-off is `event_tick > local_tick` (strict), the same
    /// asymmetric interval that keeps a note ending exactly on the loop
    /// boundary from being lost; when resuming exactly at that note-off's
    /// tick, that test always rejects it, so the caller's very first
    /// window check failed immediately and stopped, before ever
    /// reaching the note-on that starts at the same tick, right after
    /// it in the sorted list. This skips past a note-off at exactly
    /// `tick` (not eligible to be sent again), while still landing on a
    /// note-on at exactly `tick` (eligible), matching
    /// [`schedule_events`]'s own two window tests exactly.
    pub fn index_at_or_after(&self, tick: f64) -> usize {
        self.events.partition_point(|event| {
            let event_tick = event.tick as f64;
            match event.kind {
                ScheduledEventKind::NoteOn { .. } => event_tick < tick,
                ScheduledEventKind::NoteOff => event_tick <= tick,
            }
        })
    }
}

/// Computes the events `playable` produces while its playhead moves
/// from `start_tick` for `num_samples` samples, at a rate of
/// `samples_per_tick` samples per tick, calling `emit` once per event,
/// in the order they must be sent. Updates `active` to match. Returns
/// the new playhead position (wrapped to `0..loop_length_ticks`).
///
/// `cursor` is an index into `playable`'s event list. The caller keeps
/// it between calls: this function only ever advances it, or wraps it
/// back to 0 at the loop boundary, so a normal (non-discontinuous)
/// call never scans notes that are not about to play. After a
/// discontinuity, set `*cursor` with
/// [`PlayableClip::index_at_or_after`] before calling this.
///
/// `start_tick` must already be wrapped to `0..loop_length_ticks`, and
/// `*cursor` must already point at (or before) the first event at or
/// after `start_tick`.
///
/// This function has no side effects beyond `*cursor`, `active`, and
/// calls to `emit`: it does not touch a host, a clock, or the
/// allocator, so it is safe to call from the audio thread, and safe to
/// call from a plain unit test.
pub fn schedule_events(
    playable: &PlayableClip,
    cursor: &mut usize,
    start_tick: f64,
    samples_per_tick: f64,
    num_samples: usize,
    active: &mut ActiveNotes,
    emit: &mut impl FnMut(ScheduledEvent),
) -> f64 {
    if playable.is_empty() || playable.loop_length_ticks == 0 {
        return start_tick;
    }

    let loop_len_ticks = playable.loop_length_ticks as f64;
    let mut local_tick = start_tick;
    let mut block_offset = 0u32;
    let mut remaining = num_samples;

    while remaining > 0 {
        let ticks_left_in_loop = (loop_len_ticks - local_tick).max(0.0);
        let samples_left_in_loop = (ticks_left_in_loop * samples_per_tick).ceil() as usize;
        let chunk = remaining.min(samples_left_in_loop.max(1));
        let tick_end = local_tick + chunk as f64 / samples_per_tick;

        // `chunk` is capped by `ticks_left_in_loop` above, so this
        // window never spans more than one full loop, and can only
        // ever touch each event in `playable` once. A review found a
        // real memory-leak bug here: without this cap, once `*cursor`
        // wraps from the end of the list back to 0 (which happens on
        // every call whose window reaches exactly the loop boundary,
        // including the very first `process()` call, since playback
        // starts at tick 0), the loop re-checks the same
        // `[local_tick, tick_end)` window against the events it
        // already emitted moments ago in this same call. Those events
        // still pass the same `in_window` test that admitted them the
        // first time, so it emitted them again, wrapped again, and
        // never terminated: an infinite loop that handed the host an
        // unbounded stream of note events from the audio thread,
        // which is what made the plugin's memory use grow without
        // bound as soon as it started. Counting how many events this
        // call has visited, and stopping once that reaches the total
        // number of events, closes this without any allocation, so it
        // stays safe to call from the audio thread.
        let mut visited = 0usize;
        loop {
            if visited >= playable.events.len() {
                break;
            }
            if *cursor >= playable.events.len() {
                *cursor = 0;
            }
            let event = playable.events[*cursor];
            let event_tick = event.tick as f64;

            // Same asymmetric interval as before: a note start is
            // `[local_tick, tick_end)`, a note end is
            // `(local_tick, tick_end]`, so a note ending exactly on
            // the loop boundary is not lost.
            let in_window = match event.kind {
                ScheduledEventKind::NoteOn { .. } => {
                    event_tick >= local_tick && event_tick < tick_end
                }
                ScheduledEventKind::NoteOff => event_tick > local_tick && event_tick <= tick_end,
            };
            if !in_window {
                break;
            }

            let timing =
                block_offset + ((event_tick - local_tick) * samples_per_tick).round() as u32;
            let timing = timing.min(num_samples.saturating_sub(1) as u32);
            match event.kind {
                ScheduledEventKind::NoteOn { .. } => active.mark_on(event.channel, event.pitch),
                ScheduledEventKind::NoteOff => active.mark_off(event.channel, event.pitch),
            }
            emit(ScheduledEvent {
                timing,
                channel: event.channel,
                pitch: event.pitch,
                kind: event.kind,
            });
            *cursor += 1;
            visited += 1;
        }

        remaining -= chunk;
        block_offset += chunk as u32;
        local_tick += chunk as f64 / samples_per_tick;
        if local_tick >= loop_len_ticks {
            local_tick -= loop_len_ticks;
        }
    }

    local_tick
}

#[cfg(test)]
mod tests {
    use super::*;
    use composer_core::midi::{Note, TimeSignature};

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

    fn collect_events(
        playable: &PlayableClip,
        cursor: &mut usize,
        start_tick: f64,
        samples_per_tick: f64,
        num_samples: usize,
        active: &mut ActiveNotes,
    ) -> (Vec<ScheduledEvent>, f64) {
        let mut events = Vec::new();
        let next_tick = schedule_events(
            playable,
            cursor,
            start_tick,
            samples_per_tick,
            num_samples,
            active,
            &mut |event| events.push(event),
        );
        (events, next_tick)
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
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        let mut cursor = 0;
        // This clip's one note is also its whole loop (960 ticks): 1
        // sample per tick, so the loop needs 960 samples. Asking for
        // exactly that many makes this call's window span the whole
        // loop, which is the exact shape that used to make
        // `schedule_events` loop forever; see this module's docs.
        let (events, _) = collect_events(&playable, &mut cursor, 0.0, 1.0, 960, &mut active);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].timing, 0);
        assert!(matches!(events[0].kind, ScheduledEventKind::NoteOn { .. }));
        assert_eq!(events[1].timing, 959);
        assert!(matches!(events[1].kind, ScheduledEventKind::NoteOff));
        // The note-off already closed it, in the same call.
        assert!(!active.is_active(0, 60));
    }

    #[test]
    fn a_chunk_spanning_exactly_one_loop_never_emits_more_than_once_per_event() {
        // Regression test for a real memory-leak bug a review found: a
        // call whose window spans exactly one full loop (here, every
        // one of several consecutive calls, each exactly one loop long,
        // the same shape as one `process()` call per host buffer) used
        // to make `schedule_events` wrap its cursor and re-emit the
        // same events forever, without ever returning. If the fix
        // regresses, this test hangs instead of failing a normal
        // assertion; see this module's docs for why that shape triggers
        // it.
        let (clip, loop_len) = arpeggio(960);
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        let mut cursor = 0;
        let num_samples = loop_len as usize;

        for lap in 0..5 {
            let (events, next_tick) =
                collect_events(&playable, &mut cursor, 0.0, 1.0, num_samples, &mut active);
            assert_eq!(
                events.len(),
                8,
                "lap {lap}: 4 notes, one on and one off each, no more"
            );
            assert_eq!(next_tick, 0.0, "a full loop always wraps back to tick 0");
            assert_eq!(active.len(), 0, "lap {lap}: every note-on was matched");
        }
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
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        let mut cursor = 0;
        // Only ask for the first half of the note. Its note-on should
        // fire, but not its note-off yet.
        let (events, next_tick) =
            collect_events(&playable, &mut cursor, 0.0, 1.0, 480, &mut active);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].kind, ScheduledEventKind::NoteOn { .. }));
        assert!(active.is_active(0, 60));
        assert_eq!(next_tick, 480.0);
    }

    #[test]
    fn loop_boundary_note_off_is_not_lost() {
        // This is the regression test for the bug found in an earlier
        // review: the arpeggio's last note ends exactly on the loop
        // boundary. Ask for two full loops in one call, which forces
        // the scheduler to wrap around mid-call, and check every note
        // gets both its on and its off, in both loops.
        let (clip, loop_len) = arpeggio(960);
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        let mut cursor = 0;
        let num_samples = (loop_len * 2) as usize;
        let (events, next_tick) =
            collect_events(&playable, &mut cursor, 0.0, 1.0, num_samples, &mut active);

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
        let (clip, _loop_len) = arpeggio(960);
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        // Start exactly on the third note (tick 1920), and ask for
        // just enough samples to reach the loop boundary. Relocate the
        // cursor with binary search first, the way `lib.rs` does after
        // a discontinuity.
        let mut cursor = playable.index_at_or_after(1920.0);
        let (events, next_tick) =
            collect_events(&playable, &mut cursor, 1920.0, 1.0, 1920, &mut active);

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
        let playable = PlayableClip::from_clip(&clip);
        let mut active = ActiveNotes::default();
        let mut cursor = 0;
        let (events, _) = collect_events(&playable, &mut cursor, 0.0, 1.0, 1920, &mut active);

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

        let mut events = Vec::new();
        stop_all_notes(&mut active, &mut |event| events.push(event));

        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|e| e.timing == 0 && matches!(e.kind, ScheduledEventKind::NoteOff)));
        assert_eq!(active.len(), 0);
    }

    #[test]
    fn stop_all_notes_on_empty_set_is_a_no_op() {
        let mut active = ActiveNotes::default();
        let mut events = Vec::new();
        stop_all_notes(&mut active, &mut |event| events.push(event));
        assert!(events.is_empty());
    }

    #[test]
    fn active_notes_counts_overlapping_instances_of_the_same_key() {
        // A review found that the earlier `Vec`-based `ActiveNotes`
        // could not tell two overlapping notes of the same channel
        // and pitch apart from one: the second `mark_on` was a no-op,
        // and a single `mark_off` cleared it entirely. Counting fixes
        // this: both must be turned off before the key is inactive.
        let mut active = ActiveNotes::default();
        active.mark_on(0, 60);
        active.mark_on(0, 60);
        assert!(active.is_active(0, 60));

        active.mark_off(0, 60);
        assert!(
            active.is_active(0, 60),
            "one overlapping note is still active"
        );

        active.mark_off(0, 60);
        assert!(!active.is_active(0, 60));
    }

    #[test]
    fn index_at_or_after_finds_the_first_matching_event() {
        let (clip, _) = arpeggio(960);
        let playable = PlayableClip::from_clip(&clip);
        // The third note starts at tick 1920. Its note-on is the
        // first event at or after that tick.
        let index = playable.index_at_or_after(1920.0);
        assert_eq!(playable.events[index].tick, 1920);
        assert!(matches!(
            playable.events[index].kind,
            ScheduledEventKind::NoteOn { .. }
        ));
    }
}
