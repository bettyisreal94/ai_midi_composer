//! The background task seam between the editor and generation work.
//!
//! A review after Phase 3 (see `REVIEW.md`) found that the Generate
//! button called the stub generator directly, inline, on the UI
//! thread, with nothing in place for Phase 4 and Phase 5 to run a real
//! network call without blocking the UI. This module gives them a
//! ready seam: submit a prompt, get a request ID back, and poll for a
//! result under that ID later. The actual generation is still the
//! Phase 3 stub; only the plumbing around it is new.
//!
//! [`GenerateTask`], the type plugged into `Plugin::BackgroundTask`,
//! carries only a request ID, not the prompt text. This keeps it cheap
//! to copy and queue, and keeps the one copy of the prompt's heap data
//! in [`GenerationStore`], not duplicated into the task queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::midi::MidiClip;

pub type RequestId = u64;

/// The task `nih_plug`'s background thread pool runs. See the module
/// docs for why this only carries an ID.
#[derive(Debug, Clone, Copy)]
pub struct GenerateTask(pub RequestId);

/// The stub behind the "Generate" button. A real prompt-to-MIDI call
/// (Phase 4 and Phase 5) will replace this. For now, it always returns
/// a fixed demo clip, and only fails when the prompt is empty, so the
/// error path has a real, testable way to trigger.
fn generate_stub(prompt: &str) -> Result<MidiClip, String> {
    if prompt.trim().is_empty() {
        return Err("Type a prompt first.".to_string());
    }
    Ok(crate::demo_clip())
}

#[derive(Default)]
struct Requests {
    next_id: RequestId,
    prompts: HashMap<RequestId, String>,
    results: HashMap<RequestId, Result<MidiClip, String>>,
}

/// Shared between the editor (UI thread) and the background task
/// executor (a `nih_plug` worker thread). Never touched by the
/// real-time audio callback.
///
/// Only one request needs to be tracked at a time in this phase,
/// because the editor disables "Generate" while a request is pending
/// (see `agent_ui::draw`'s `generation_pending` parameter). That also
/// means a "stale response from an old request" can never happen here:
/// there is never more than one outstanding request to begin with. A
/// future phase that allows several requests at once (for example, a
/// cancel button) would need to compare IDs when polling, to drop a
/// response that arrives after a newer request replaced it.
#[derive(Clone, Default)]
pub struct GenerationStore(Arc<Mutex<Requests>>);

impl GenerationStore {
    /// Records a new prompt, and returns the request ID for it.
    pub fn submit(&self, prompt: String) -> RequestId {
        let mut requests = self.0.lock().unwrap();
        let id = requests.next_id;
        requests.next_id += 1;
        requests.prompts.insert(id, prompt);
        id
    }

    /// Runs the stub generator for `id`'s prompt, and stores the
    /// result. Call this from the background task executor, not from
    /// the UI or audio thread. Does nothing if `id` is unknown.
    pub fn run(&self, id: RequestId) {
        let prompt = {
            let requests = self.0.lock().unwrap();
            match requests.prompts.get(&id) {
                Some(prompt) => prompt.clone(),
                None => return,
            }
        };
        let result = generate_stub(&prompt);
        let mut requests = self.0.lock().unwrap();
        requests.prompts.remove(&id);
        requests.results.insert(id, result);
    }

    /// Removes and returns the result for `id`, if it is ready.
    pub fn poll(&self, id: RequestId) -> Option<Result<MidiClip, String>> {
        self.0.lock().unwrap().results.remove(&id)
    }
}
