//! Runs the native file dialog, and the file I/O that follows it, off
//! the editor's own paint callback, on a plain spawned thread.
//!
//! A maintainer hit a real crash pressing "Save as .mid..." for the
//! first time in a real DAW, on macOS. `crates/composer-plugin/src/lib.rs`
//! already wraps the whole editor callback in `std::panic::catch_unwind`
//! as a first mitigation, but that cannot catch this: it is not a Rust
//! panic. `rfd`'s blocking dialog functions (`FileDialog::save_file`,
//! `FileDialog::pick_file`) show their native panel by dispatching onto
//! the OS main thread and blocking the calling thread until the user is
//! done with it; on macOS that dispatch is a `dispatch_sync` onto the
//! main queue. The editor's paint callback already runs on the main
//! thread (it draws through AppKit, via `egui-baseview`), so calling
//! `rfd` directly from inside it makes that `dispatch_sync` target the
//! very queue that is already running it, which is a guaranteed GCD
//! deadlock. In practice that surfaces as a hang or a hard abort, not a
//! catchable Rust panic. Running the dialog and the read or write that
//! follows it on a plain spawned thread instead avoids this: the
//! calling thread is then a background thread, not the main queue, so
//! `rfd`'s own main-queue dispatch can actually complete.
//!
//! This intentionally does not go through `nih_plug`'s own
//! `BackgroundTask`/`AsyncExecutor` machinery (see `background.rs`):
//! that seam exists so audio-thread code can hand work to a host-run
//! thread pool. Nothing here is ever submitted from the audio thread,
//! so a plain `std::thread::spawn` is simpler, and keeps this module
//! independent of `Plugin::BackgroundTask`.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use composer_core::midi::MidiClip;

pub type RequestId = u64;

#[derive(Default)]
struct Requests {
    next_id: RequestId,
    save_results: HashMap<RequestId, Result<String, String>>,
    load_results: HashMap<RequestId, Result<Option<MidiClip>, String>>,
}

/// Shared between the editor (UI thread) and the background threads
/// this spawns. Never touched by the real-time audio callback.
///
/// Only one dialog needs to be tracked at a time in this phase, because
/// the editor disables "Load .mid..." and "Save as .mid..." while a
/// dialog is already pending (see `composer_ui::draw`'s `dialog_pending`
/// parameter).
#[derive(Clone, Default)]
pub struct DialogStore(Arc<Mutex<Requests>>);

impl DialogStore {
    /// Opens a native "save file" dialog on a spawned thread, and
    /// writes `clip` to the chosen path as a standard MIDI file if the
    /// user picks one. See the module docs for why this must not run on
    /// the calling (UI) thread. Returns a request ID to poll with
    /// [`Self::poll_save`].
    pub fn submit_save(&self, clip: MidiClip) -> RequestId {
        let id = self.next_id();
        let store = self.clone();
        std::thread::spawn(move || {
            let result = save_clip_to_file(&clip);
            store.0.lock().unwrap().save_results.insert(id, result);
        });
        id
    }

    /// Opens a native "open file" dialog on a spawned thread, and reads
    /// the chosen path as a standard MIDI file if the user picks one.
    /// See the module docs for why this must not run on the calling
    /// (UI) thread. Returns a request ID to poll with
    /// [`Self::poll_load`].
    pub fn submit_load(&self) -> RequestId {
        let id = self.next_id();
        let store = self.clone();
        std::thread::spawn(move || {
            let result = load_clip_from_file();
            store.0.lock().unwrap().load_results.insert(id, result);
        });
        id
    }

    fn next_id(&self) -> RequestId {
        let mut requests = self.0.lock().unwrap();
        let id = requests.next_id;
        requests.next_id += 1;
        id
    }

    /// Removes and returns the result for `id`'s save request, if it is
    /// ready.
    pub fn poll_save(&self, id: RequestId) -> Option<Result<String, String>> {
        self.0.lock().unwrap().save_results.remove(&id)
    }

    /// Removes and returns the result for `id`'s load request, if it is
    /// ready.
    pub fn poll_load(&self, id: RequestId) -> Option<Result<Option<MidiClip>, String>> {
        self.0.lock().unwrap().load_results.remove(&id)
    }
}

/// Opens a native "save file" dialog, and writes `clip` to the chosen
/// path as a standard MIDI file. Returns a short message describing
/// what happened, so the editor can show it. Returns `Ok` with an empty
/// message if the user cancels the dialog, since that is not a failure.
///
/// Must run on a background thread, not the UI thread; see the module
/// docs.
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
/// with no limit at all, which meant an accidental or hostile
/// multi-gigabyte file could stall this background thread and exhaust
/// memory. This does not eliminate that risk (a picked file is normally
/// small enough that the read is fast, and this now runs off the UI
/// thread regardless), but it does stop an oversized file from ever
/// being read into memory in the first place.
const MAX_MIDI_FILE_BYTES: u64 = 5_000_000;

/// Opens a native "open file" dialog, and reads the chosen path as a
/// standard MIDI file, using the Phase 1 code
/// (`MidiClip::from_smf_bytes`). Returns `Ok(None)` if the user cancels
/// the dialog, since that is not a failure.
///
/// Must run on a background thread, not the UI thread; see the module
/// docs.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_save_assigns_a_different_id_to_each_request() {
        let store = DialogStore::default();
        let first = store.next_id();
        let second = store.next_id();
        assert_ne!(first, second);
    }

    #[test]
    fn poll_save_returns_none_before_a_result_exists() {
        let store = DialogStore::default();
        assert!(store.poll_save(0).is_none());
    }

    #[test]
    fn poll_save_delivers_a_result_exactly_once() {
        // Inserts a result directly, bypassing the spawned thread, so
        // this test does not depend on a real native dialog.
        let store = DialogStore::default();
        store
            .0
            .lock()
            .unwrap()
            .save_results
            .insert(0, Ok("Saved to /tmp/x.mid.".to_string()));

        assert!(store.poll_save(0).is_some());
        assert!(
            store.poll_save(0).is_none(),
            "a result should only be delivered once"
        );
    }

    #[test]
    fn poll_load_delivers_a_result_exactly_once() {
        let store = DialogStore::default();
        store.0.lock().unwrap().load_results.insert(0, Ok(None));

        assert!(store.poll_load(0).is_some());
        assert!(
            store.poll_load(0).is_none(),
            "a result should only be delivered once"
        );
    }
}
