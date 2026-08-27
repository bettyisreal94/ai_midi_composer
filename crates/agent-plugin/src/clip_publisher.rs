//! A real-time-safe way to hand a freshly generated [`MidiClip`] to the
//! audio thread, for live playback.
//!
//! Phase 5's plan (see `TODO.md`) explicitly ruled out the obvious
//! design: a plain channel of owned `MidiClip` values. A `MidiClip`
//! owns a `Vec<Note>` on the heap. If the audio thread received a new
//! `MidiClip` from a channel and assigned it over the old one, dropping
//! the old value, and its `Vec`, would happen on the audio thread,
//! which can call into the allocator at an unpredictable time and is
//! not real-time safe.
//!
//! This module uses a triple buffer instead (the `triple_buffer`
//! crate). A [`ClipPublisher`] (used by the editor and the background
//! task executor) always writes a brand new value into its own
//! private "back" buffer, which drops whatever was in that slot
//! before, on the *publisher's* thread. A [`ClipReader`] (used by the
//! audio thread) only ever swaps which already-allocated buffer is
//! "front", and reads a reference into it: no allocation, and no
//! `Drop`, ever happens on the audio thread.
//!
//! Each published clip carries a `generation` number, so
//! `agent-plugin`'s `process()` can tell when the clip actually
//! changed (not just when the host's transport moved), and clean up
//! and resynchronize accordingly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agent_core::midi::MidiClip;

/// One published clip, tagged with a generation number.
#[derive(Clone)]
pub struct PublishedClip {
    pub clip: MidiClip,
    pub generation: u64,
}

/// The audio thread's side. Owned by `AgentPlugin`; never cloned or
/// shared, since a triple buffer has exactly one reader.
pub struct ClipReader {
    output: triple_buffer::Output<PublishedClip>,
}

impl ClipReader {
    /// Returns a reference to the most recently published clip. Never
    /// allocates, and never drops a value: safe to call from the audio
    /// thread.
    pub fn read(&mut self) -> &PublishedClip {
        self.output.read()
    }
}

/// The producer's side: the editor, and the background task executor
/// that runs a real generation request. Cheap to clone; every clone
/// shares the same underlying buffer.
#[derive(Clone)]
pub struct ClipPublisher {
    input: Arc<Mutex<triple_buffer::Input<PublishedClip>>>,
    next_generation: Arc<AtomicU64>,
}

impl ClipPublisher {
    /// Builds a new publisher/reader pair, with `initial_clip` already
    /// published as generation 0.
    pub fn new(initial_clip: MidiClip) -> (Self, ClipReader) {
        let (input, output) = triple_buffer::TripleBuffer::new(&PublishedClip {
            clip: initial_clip,
            generation: 0,
        })
        .split();

        let publisher = Self {
            input: Arc::new(Mutex::new(input)),
            next_generation: Arc::new(AtomicU64::new(1)),
        };
        (publisher, ClipReader { output })
    }

    /// Publishes `clip` as the new value for the audio thread to pick
    /// up. Call this from the editor or the background task executor,
    /// never from the audio thread.
    pub fn publish(&self, clip: MidiClip) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.input
            .lock()
            .expect("clip publisher mutex should not be poisoned")
            .write(PublishedClip { clip, generation });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::midi::{Note, TimeSignature};

    fn one_note_clip(pitch: u8) -> MidiClip {
        MidiClip {
            ticks_per_quarter: 960,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: vec![Note {
                pitch,
                velocity: 100,
                start: 0,
                duration: 480,
                channel: 0,
            }],
        }
    }

    #[test]
    fn reader_sees_the_initial_clip_at_generation_zero() {
        let (_publisher, mut reader) = ClipPublisher::new(one_note_clip(60));
        let published = reader.read();
        assert_eq!(published.generation, 0);
        assert_eq!(published.clip.notes[0].pitch, 60);
    }

    #[test]
    fn reader_sees_a_published_update_with_a_new_generation() {
        let (publisher, mut reader) = ClipPublisher::new(one_note_clip(60));
        publisher.publish(one_note_clip(64));

        let published = reader.read();
        assert_eq!(published.generation, 1);
        assert_eq!(published.clip.notes[0].pitch, 64);
    }

    #[test]
    fn generation_numbers_keep_increasing_across_several_publishes() {
        let (publisher, mut reader) = ClipPublisher::new(one_note_clip(60));
        publisher.publish(one_note_clip(62));
        publisher.publish(one_note_clip(64));
        publisher.publish(one_note_clip(67));

        let published = reader.read();
        assert_eq!(published.generation, 3);
        assert_eq!(published.clip.notes[0].pitch, 67);
    }

    #[test]
    fn cloned_publishers_share_the_same_buffer() {
        let (publisher, mut reader) = ClipPublisher::new(one_note_clip(60));
        let cloned = publisher.clone();
        cloned.publish(one_note_clip(69));

        let published = reader.read();
        assert_eq!(published.clip.notes[0].pitch, 69);
    }
}
