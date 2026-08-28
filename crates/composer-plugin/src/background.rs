//! The background task seam between the editor and generation work.
//!
//! A review after Phase 3 (see `REVIEW.md`) found that the Generate
//! button called the stub generator directly, inline, on the UI
//! thread, with nothing in place for Phase 4 and Phase 5 to run a real
//! network call without blocking the UI. This module gives them a
//! ready seam: submit a prompt and the provider settings to use, get a
//! request ID back, and poll for a result under that ID later. Phase 5
//! plugged a real call to `composer_core::generate_clip` in here, in
//! place of the Phase 3 stub. Phase 6 added an optional existing clip
//! to a request: when present, `run()` calls
//! `composer_core::generate_variation` instead, to vary that clip by the
//! prompt's instruction, rather than generating a new one from
//! scratch.
//!
//! [`GenerateTask`], the type plugged into `Plugin::BackgroundTask`,
//! carries only a request ID, not the prompt text. This keeps it cheap
//! to copy and queue, and keeps the one copy of the prompt's heap data
//! in [`GenerationStore`], not duplicated into the task queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use composer_core::midi::MidiClip;
use composer_core::{AiProvider, AnthropicProvider, OpenAiCompatibleProvider};

pub type RequestId = u64;

/// The task `nih_plug`'s background thread pool runs. See the module
/// docs for why this only carries an ID.
#[derive(Debug, Clone, Copy)]
pub struct GenerateTask(pub RequestId);

/// A snapshot of the provider settings a request should use, taken at
/// the moment the user pressed "Generate". Settings the user changes
/// afterward, while the request is still running, do not affect it.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub kind: composer_ui::ProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl From<&composer_ui::ProviderSettings> for ProviderConfig {
    fn from(settings: &composer_ui::ProviderSettings) -> Self {
        Self {
            kind: settings.kind,
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            model: settings.model.clone(),
        }
    }
}

fn build_provider(config: &ProviderConfig) -> Box<dyn AiProvider> {
    match config.kind {
        composer_ui::ProviderKind::OpenAiCompatible => Box::new(OpenAiCompatibleProvider::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
        )),
        composer_ui::ProviderKind::Anthropic => Box::new(AnthropicProvider::with_base_url(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
        )),
    }
}

/// A submitted, not yet run, request.
#[derive(Clone)]
struct PendingRequest {
    prompt: String,
    config: ProviderConfig,
    /// `Some` for a "vary this clip" request (Phase 6); `None` for a
    /// "generate something new" request.
    existing_clip: Option<MidiClip>,
}

#[derive(Default)]
struct Requests {
    next_id: RequestId,
    pending: HashMap<RequestId, PendingRequest>,
    results: HashMap<RequestId, Result<MidiClip, String>>,
}

/// Shared between the editor (UI thread) and the background task
/// executor (a `nih_plug` worker thread). Never touched by the
/// real-time audio callback.
///
/// Only one request needs to be tracked at a time in this phase,
/// because the editor disables "Generate" while a request is pending
/// (see `composer_ui::draw`'s `generation_pending` parameter). That also
/// means a "stale response from an old request" can never happen here:
/// there is never more than one outstanding request to begin with. A
/// future phase that allows several requests at once (for example, a
/// cancel button) would need to compare IDs when polling, to drop a
/// response that arrives after a newer request replaced it.
#[derive(Clone, Default)]
pub struct GenerationStore(Arc<Mutex<Requests>>);

impl GenerationStore {
    /// Records a new request, and returns the request ID for it.
    /// `existing_clip` is `Some` for a "vary this clip" request, where
    /// `prompt` is the instruction to vary it by, or `None` for a
    /// "generate something new" request, where `prompt` is the whole
    /// request.
    pub fn submit(
        &self,
        prompt: String,
        config: ProviderConfig,
        existing_clip: Option<MidiClip>,
    ) -> RequestId {
        let mut requests = self.0.lock().unwrap();
        let id = requests.next_id;
        requests.next_id += 1;
        requests.pending.insert(
            id,
            PendingRequest {
                prompt,
                config,
                existing_clip,
            },
        );
        id
    }

    /// Runs the real prompt-to-MIDI pipeline for `id`'s request, and
    /// stores the result. Call this from the background task executor,
    /// not from the UI or audio thread: it makes a blocking HTTP call.
    /// Does nothing if `id` is unknown.
    pub fn run(&self, id: RequestId) {
        let request = {
            let requests = self.0.lock().unwrap();
            match requests.pending.get(&id) {
                Some(entry) => entry.clone(),
                None => return,
            }
        };

        let provider = build_provider(&request.config);
        let result = match &request.existing_clip {
            Some(existing_clip) => {
                composer_core::generate_variation(provider.as_ref(), existing_clip, &request.prompt)
            }
            None => composer_core::generate_clip(provider.as_ref(), &request.prompt),
        }
        .map_err(|err| err.to_string());

        let mut requests = self.0.lock().unwrap();
        requests.pending.remove(&id);
        requests.results.insert(id, result);
    }

    /// Removes and returns the result for `id`, if it is ready.
    pub fn poll(&self, id: RequestId) -> Option<Result<MidiClip, String>> {
        self.0.lock().unwrap().results.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ProviderConfig {
        ProviderConfig {
            kind: composer_ui::ProviderKind::OpenAiCompatible,
            base_url: "http://localhost".to_string(),
            api_key: String::new(),
            model: "test-model".to_string(),
        }
    }

    #[test]
    fn submit_assigns_a_different_id_to_each_request() {
        let store = GenerationStore::default();
        let first = store.submit("a".to_string(), config(), None);
        let second = store.submit("b".to_string(), config(), None);
        assert_ne!(first, second);
    }

    #[test]
    fn poll_returns_none_before_a_result_exists() {
        let store = GenerationStore::default();
        let id = store.submit("a".to_string(), config(), None);
        assert!(store.poll(id).is_none());
    }

    #[test]
    fn poll_delivers_a_result_exactly_once() {
        // Inserts a result directly, bypassing `run()`, so this test
        // does not need a real network call: `run()`'s own behavior is
        // covered by `composer_core::pipeline`'s tests instead.
        let store = GenerationStore::default();
        let id = store.submit("a".to_string(), config(), None);
        store
            .0
            .lock()
            .unwrap()
            .results
            .insert(id, Err("boom".to_string()));

        assert!(store.poll(id).is_some());
        assert!(
            store.poll(id).is_none(),
            "a result should only be delivered once"
        );
    }

    #[test]
    fn run_on_an_unknown_id_does_nothing() {
        let store = GenerationStore::default();
        store.run(999);
        assert!(store.poll(999).is_none());
    }

    #[test]
    fn submit_records_an_existing_clip_for_a_variation_request() {
        use composer_core::midi::{MidiClip, Note, TimeSignature};

        let store = GenerationStore::default();
        let clip = MidiClip {
            ticks_per_quarter: 960,
            tempo_bpm: 120.0,
            time_signature: TimeSignature::default(),
            notes: vec![Note {
                pitch: 60,
                velocity: 100,
                start: 0,
                duration: 480,
                channel: 0,
            }],
        };
        let id = store.submit("add a harmony line".to_string(), config(), Some(clip));

        let requests = store.0.lock().unwrap();
        let pending = requests
            .pending
            .get(&id)
            .expect("request should be pending");
        assert!(pending.existing_clip.is_some());
        assert_eq!(pending.prompt, "add a harmony line");
    }
}
