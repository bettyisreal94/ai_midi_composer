# TODO: Open Source AI MIDI Generator Plugin

This file is a plan. It uses Simplified Technical English. Sentences are
short. Each sentence has one idea.

## 1. Goal

Build an open source plugin. The plugin creates MIDI data from text prompts.
The plugin uses AI language models to do this. The plugin must run on Linux.
The plugin must run on macOS. The plugin must be written in Rust.

## 2. Reference product

The reference product is MIDI Agent (https://www.midiagent.com/). MIDI Agent
is a paid, closed source plugin. It has these features:

- The user types a text prompt. The plugin sends the prompt to an AI model.
  The AI model returns MIDI notes. The notes can be a melody, a chord
  progression, a drum pattern, or a bass line.
- The user can load an existing MIDI file. The plugin can extend the file.
  The plugin can create variations of the file.
- The user can load an audio file. The plugin converts the audio to MIDI
  notes. This step is called audio-to-MIDI transcription.
- The plugin outputs MIDI data, not audio. The user can edit the notes in
  the DAW piano roll.
- The plugin supports many AI providers: OpenAI, Anthropic Claude, Google
  Gemini, xAI Grok, DeepSeek, OpenRouter, and local models through Ollama or
  LM Studio.
- The plugin ships as VST3, AU, AAX, and a standalone app.

## 3. Scope for version 1

Version 1 must have these features. A review after Phase 6 found that
these boxes stayed unchecked even after the phase that built each
feature was already checked in section 8, with no note about why. This
section's checkbox now means one specific thing: the feature is built
and covered by automated tests. It does not mean every manual,
real-provider, or real-DAW check for that feature has also run yet;
section 9's manual test checklist, and each phase's own open items in
section 8, track those separately, and stay unchecked until a
maintainer runs them by hand.

- [x] Text-to-MIDI generation. The user types a prompt. The plugin returns
      MIDI notes.
  - Built in Phase 5. Typing a real prompt against a real provider has
    not been run yet; see Phase 5 and section 9.
- [x] MIDI file import. The user loads a MIDI file. The plugin can extend it
      or create a variation of it.
  - Built in Phase 6.
- [x] Support for OpenAI-compatible chat APIs. This covers OpenAI, DeepSeek,
      OpenRouter, Ollama, and LM Studio, because they use the same API
      shape.
  - Built in Phase 4. Tested against a local mock server only, not
    against any of these real services yet; see Phase 4 and section 9.
- [x] Support for the Anthropic Claude API.
  - Built in Phase 4, tested the same way as the item above.
- [x] A user interface inside the plugin window. The interface has a text
      box, a generate button, and a provider settings panel.
  - Built in Phase 3, Phase 3.5, and Phase 4.
- [x] Plugin formats: CLAP and VST3.
  - Built in Phase 0 and Phase 2.
- [ ] Platforms: Linux and macOS.
  - macOS is confirmed with `pluginval` (see Phase 2). Linux has not
    been confirmed at all yet: no Linux machine has been available.
    This stays unchecked until that happens.
- [x] Local storage of API keys. Keys must not appear in plain log files.
  - Built in Phase 4, hardened after a review; see Phase 4. Not yet
    confirmed against a real macOS Keychain prompt or a real Linux
    Secret Service provider; see section 11.

## 4. Out of scope for version 1

These features come later, or may not come at all:

- [ ] Audio-to-MIDI transcription. This needs a trained model. This is a
      separate, larger effort. Plan it as phase 8.
- [ ] Google Gemini and xAI Grok support. Add these after Claude support
      works.
- [ ] AAX format. AAX needs a license from Avid. A license has legal terms
      that may not fit an open source project. Skip AAX in version 1.
- [ ] AU (Audio Unit) format. AU is needed for Logic Pro and GarageBand.
      Rust tools for AU are not mature. Revisit this after CLAP and VST3
      work well.
- [ ] Windows support. The user asked for Linux and macOS only.
- [ ] A hosted subscription service, like "MIDI Agent Pro". This project
      only wraps APIs that the user supplies.

## 5. Architecture overview

The project has three layers:

1. **Core library** (`agent-core`). This crate has no plugin code. It has
   the MIDI data model, the AI provider clients, and the prompt templates.
   Other Rust programs can reuse this crate.
2. **Plugin crate** (`agent-plugin`). This crate uses `agent-core`. It
   builds the CLAP and VST3 plugin. It has the audio thread code and the
   plugin parameters.
3. **UI crate** (`agent-ui`). This crate draws the plugin window. It sends
   user actions to the plugin crate. It shows results from the plugin
   crate.

A review after Phase 3 (see `REVIEW.md`) found that this section's plan
contradicted itself: it recommended `nih_plug`'s own background task
mechanism, while section 6 still listed `tokio` as the crate that owns
the background thread. Phase 3 also had no seam at all for background
work: the "Generate" button called the stub generator inline, on the UI
thread. Both are fixed now. This section describes what is actually
built, not just a plan to check future code against.

**Background work.** `agent-plugin` uses `nih_plug`'s own background
task mechanism: `Plugin::BackgroundTask`, `Plugin::task_executor()`, and
`AsyncExecutor::execute_background()`. There is no separate `tokio`
runtime, and Phase 4's real provider calls should use a blocking HTTP
client (`reqwest`'s `blocking` feature, not its default async API)
inside the background task closure, for the same reason: one thread
pool to reason about, not two. The background task type
(`background::GenerateTask`) carries only a request ID, not the prompt
text, so queuing a task never duplicates heap-owned request data. The
prompt, and the result, live in `background::GenerationStore`, a small
`Arc<Mutex<...>>` map from request ID to prompt and to result, shared
between the editor and the task executor, and never touched by the
audio callback. The editor disables "Generate" while a request is
pending, so only one request is ever outstanding at a time; this also
means a stale response from an old request cannot happen yet. A future
phase that allows several requests at once (for example, a cancel
button) will need to compare IDs when polling, to drop a response from
a request a newer one already replaced.

**The three crates.** `agent-ui` now holds the UI's own state
(`agent_ui::EditorState`) and its rendering (`agent_ui::draw()`), and
knows nothing about `nih_plug`, request IDs, or the file system. It
returns which button the user pressed, as an `agent_ui::UiAction`
value; `agent-plugin` decides what that action means. `agent-plugin`
creates the `nih_plug_egui` editor window, owns
`background::GenerationStore`, and does the host-integration work a UI
crate should not need to know about, such as opening a native save
dialog with `rfd`.

**Live MIDI output is connected to "Generate", since Phase 5.** A
freshly generated clip reaches the audio thread through a triple
buffer (`crates/agent-plugin/src/clip_publisher.rs`), not a plain
channel of owned `MidiClip` values: a review after Phase 3 found that
replacing an owned value on the audio thread that way can still drop
its old, heap-backed `Vec<Note>` there, even behind a bounded,
non-blocking channel, because dropping the old value happens on the
audio thread the moment the new one replaces it. A triple buffer
avoids this: writing a new clip (on the editor's, or the background
task executor's, thread) is what drops the old one, and reading the
latest clip (on the audio thread) never allocates or drops a value.
Each published clip carries a generation number, so the audio thread
can tell a genuine clip replacement apart from an ordinary transport
update, and clean up and resynchronize only when the clip itself
changed. See Phase 5 for the full explanation.

```
                +----------------+
   Host DAW --> |  CLAP / VST3   |
                |  plugin shell  |
                |  (agent-plugin)|
                +----+------+----+
                     |      |
             audio   |      |  UI events
             thread  |      |
                     v      v
              +-------------------+        +----------------+
              |   agent-core      | <----> |  AI provider   |
              |  (MIDI + prompt   |  HTTP  |  (OpenAI,      |
              |   + job queue)    |        |   Claude, ...) |
              +-------------------+        +----------------+
```

## 6. Rust crates to use

- `nih_plug` — builds CLAP and VST3 plugins from one Rust codebase.
  Also runs background work off the audio and UI threads, through its
  own task executor; see section 5. No separate async runtime is
  needed for this.
- `nih_plug_egui` — draws the plugin window with `egui`.
- `egui` — the widget toolkit `agent-ui` draws with. `agent-ui` depends
  on this directly (not through `nih_plug_egui`), so it stays usable
  without pulling in `nih_plug`'s windowing backend.
- `midly` — reads and writes standard MIDI files.
- `reqwest`, with its `blocking` feature, not the default async API —
  sends HTTP requests to AI providers, from inside a `nih_plug`
  background task. See section 5 for why this is blocking, not async.
- `rfd` — opens native file dialogs, such as "Save as .mid".
- `serde` and `serde_json` — reads and writes JSON.
- `keyring` — stores API keys in the OS secure storage (Keychain on
  macOS, Secret Service on Linux).
- `triple_buffer` — hands a freshly generated `MidiClip` to the audio
  thread for live playback, without allocating or dropping a value on
  that thread. See section 5.
- `directories` — finds the correct config folder on each OS.

## 7. Repository layout

```
vst/
  Cargo.toml               # workspace file
  crates/
    agent-core/             # MIDI model; AI clients come in Phase 4
    agent-plugin/           # CLAP/VST3 plugin: host integration, real-time
                             # code, and background task execution
    agent-ui/               # the plugin window's UI state and rendering
  xtask/                    # build and bundle scripts
  TODO.md
  README.md
```

## 8. Implementation phases

### Phase 0 — Project setup
- [x] Create the Cargo workspace with the three crates above.
- [x] Add `nih_plug` as a git dependency and build an empty passthrough
      plugin.
- [x] Validate the plugin with headless tools, since no DAW is installed
      on the build machine yet:
  - [x] `pluginval` (strictness 5) on the VST3 bundle.
    - Result: SUCCESS.
  - [x] `clap-validator` on the CLAP bundle.
    - Result: 30 passed, 2 failed.
    - Both failures are tracked in section 11 (Open questions). Neither
      one blocks later phases.
- [ ] Load the plugin in a real DAW (Reaper or Bitwig) on Linux and on
      macOS.
  - Do this once a DAW is available on a test machine.
- [ ] Set up CI to build the workspace on Linux and macOS.
  - Postponed. Do this later, once the project has a git remote and the
    core features are more stable.

### Phase 1 — MIDI data model
- [x] Define a `Note` struct: pitch, velocity, start time, duration,
      channel.
  - See `crates/agent-core/src/midi.rs`.
- [x] Define a `MidiClip` struct: a list of notes plus tempo and time
      signature.
- [x] Write functions to convert a `MidiClip` to a standard MIDI file, and
      back, using `midly`.
  - These are `MidiClip::to_smf_bytes` and `MidiClip::from_smf_bytes`.
  - They build a format 0 (single track) file.
  - Reading back merges notes from every track in a file, and keeps the
    first tempo and time signature found.
- [x] Write unit tests for the conversion functions.
  - See section 9 (Testing plan) for the full list of what they cover.
- [x] Harden the model against silent data loss and invalid files.
  - A review of this phase found that the first version accepted
    silent (velocity 0) and zero-duration notes, encoded bad time
    signatures without complaint, and could lose notes when two notes
    of the same channel and pitch overlapped in time.
  - The current version rejects all of these on export, with a
    specific `MidiError` for each case, and uses a documented,
    deterministic policy (last-in-first-out matching) for overlapping
    notes found when reading a file written by another program.

### Phase 2 — Plugin skeleton with a fixed, generated MIDI clip

A review of phases 0 and 1 (see `REVIEW.md`) found a gap in this phase's
original plan. MIDI Agent's own documented workflow is: generate a clip
inside the plugin, then drag it into the DAW, or save it as a `.mid`
file. Live MIDI output, sent straight from the plugin while it plays,
is a nice extra, but it is not the main way MIDI Agent moves notes into
a project. The plan below fixes this, and adds it earlier than before.

There is also a plugin-format problem to solve here. CLAP has a clean
"note effect" plugin type, but VST3 does not. A VST3 plugin that only
outputs MIDI, with no audio, is not a shape every host expects. The
safe choice is to keep the dummy stereo audio bus this project already
has from Phase 0, and register as an instrument-like plugin in VST3,
even though the plugin does not make sound. This trades a small,
harmless oddity (an "instrument" with silent audio) for wide host
support.

- [x] Decide, and record here, the exact VST3 category for this plugin
      (for example `Vst3SubCategory::Instrument`), and the CLAP category
      (`ClapFeature::NoteEffect` or `ClapFeature::Instrument`). Keep the
      dummy stereo audio bus.
  - Decision: `Vst3SubCategory::Instrument` for VST3, and
    `ClapFeature::Instrument` (plus `ClapFeature::Stereo`, for the
    dummy audio bus) for CLAP.
  - See `crates/agent-plugin/src/lib.rs`.
- [x] Build a small, fixed `MidiClip` value inside the plugin, using the
      Phase 1 data model. This stands in for real AI generation, until
      Phase 5.
  - See `demo_clip()`: a four-note, one-octave arpeggio, looping every
    4 beats.
- [ ] Add a "drag out" control in the plugin window: the user can drag
      the generated clip from the plugin into a DAW track.
  - **Not done.** Neither `egui` nor `baseview` (the windowing crate
    `nih_plug_egui` uses) has a cross-platform way to start a native OS
    drag session from inside a plugin window.
  - Doing this for real needs platform-specific code (for example
    Cocoa's `NSDraggingSession` on macOS, and XDND on Linux X11), which
    this project has not written or tested yet.
  - "Save as .mid" is the supported way to get the clip out of the
    plugin for now. Revisit this once a maintainer can test it against
    a real DAW on both platforms.
- [x] Add a "Save as .mid" button, so the user can export the clip as a
      file, for DAWs or workflows that do not support dragging a clip
      out of a plugin window.
  - Uses the `rfd` crate for a native file save dialog.
  - Correction: an earlier version of this file said Linux needs GTK 3
    development files for this. A review after Phase 3 found that
    wrong: with `rfd`'s default features, its Linux backend uses the
    XDG desktop portal over D-Bus, not GTK 3, so no extra development
    package is needed at build time. See `README.md`'s Requirements
    section for the run-time note.
- [x] As a secondary feature, also send the clip as live MIDI output
      events, for hosts that support live MIDI from a plugin.
  - The clip loops, synced to the host's own transport position when
    the host reports one.
  - A review after Phase 3 (see `REVIEW.md`) found three real bugs in
    the first version of this scheduler, all fixed, with unit tests, in
    `crates/agent-plugin/src/scheduler.rs`:
    - A note ending exactly on the loop boundary never got its
      note-off, and stayed stuck on forever. Fixed with an asymmetric
      half-open interval for note starts versus note ends.
    - Stopping the transport, or a seek, did not send cleanup
      note-offs for notes already sounding, so they could also stay
      stuck on. Fixed by tracking active notes (`ActiveNotes`), and
      sending note-offs for all of them on stop and on a detected
      transport discontinuity.
    - A note-off and a note-on at the same sample were sent in
      whatever order the clip's notes happened to be stored in. Fixed
      by sorting scheduled events so a note-off always sorts before a
      note-on at the same sample.
  - A related bug was in how the plugin found its position in the
    clip: it converted `Transport::pos_samples()` to ticks using only
    the current tempo, which is wrong once a project has an earlier
    tempo change. Fixed by using `Transport::pos_beats()` instead,
    which the host already computes correctly across tempo changes.
  - `clap-validator`'s `transport-fuzz` and
    `transport-fuzz-sample-accurate` tests, which change the transport
    state on every block, both still pass, so the fixed scheduler
    handles erratic host transport behavior without crashing or
    producing invalid audio.
  - A real host-matrix test (which hosts accept live MIDI from a
    plugin at all) is still open; add it once a DAW is available on a
    test machine, alongside the Phase 0 DAW check.
  - A maintainer later found a critical bug in the Phase 6 rewrite of
    this same scheduler (the one described above, in section 5, that
    made it walk a precomputed event list with a cursor, instead of
    scanning and sorting notes on every audio callback): starting the
    plugin could make its memory use grow past 50 GB. Two real bugs,
    both in `crates/agent-plugin/src/scheduler.rs`, caused this,
    fixed now, with a regression test:
    - `schedule_events` wrapped its event cursor back to 0 whenever it
      reached the end of the event list, with no limit on how many
      times one call could do that. A call whose window spans exactly
      one full loop, which happens on the very first `process()` call,
      made the wrapped cursor re-check, and re-emit, the same events
      forever: an infinite loop that handed the host an unbounded
      stream of note events straight from the audio thread. Fixed with
      a plain counter, capped at the event count, since one call can
      never need to touch an event more than once.
    - `PlayableClip::index_at_or_after`, used to relocate the cursor
      after a discontinuity, could land on a note-off sitting exactly
      at the target tick. `schedule_events`'s own window test for a
      note-off always rejects that, so resuming exactly on such a tick
      silently scheduled nothing at all, forever, until the next
      discontinuity. Fixed by skipping a same-tick note-off, while
      still landing on a same-tick note-on.
    - See `crates/agent-plugin/src/scheduler.rs`'s module docs for the
      full explanation. Running this module's own unit tests, which
      hung instead of finishing, is what confirmed the first bug.
- [x] Confirm this works on macOS.
  - `pluginval` (strictness 5) reports SUCCESS, including its "Editor"
    and "Open editor whilst processing" tests.
  - `clap-validator` reports the same 30 passed / 2 failed result as
    Phase 0 and Phase 1, with no new failures (the 2 failures are the
    pre-existing, documented ones in section 11).
  - Linux is still unconfirmed, same as Phase 0: no Linux machine is
    available yet.

### Phase 3 — Basic user interface
- [x] Add a text box for the prompt.
  - A multi-line `egui` text box, in the same editor window
    `nih_plug_egui` added in Phase 2.
- [x] Add a "Generate" button.
  - Disabled while a request is pending, so the user cannot start a
    second one (see the Phase 3.5 note below).
- [x] Add a status label for "working", "done", and "error" states.
  - See `agent_ui::GenerationStatus`.
  - Unlike the first version of this phase, "working" is now real and
    reachable: pressing "Generate" submits a background task (Phase
    3.5 below), and the label shows "working…" until it finishes.
- [x] Wire the button to a stub function. The stub returns a fixed
      `MidiClip` for now.
  - See `background::generate_stub()`.
  - It also fails on an empty prompt, with a message shown through the
    "error" status, so that state has a real, testable way to trigger,
    and is not just defined and unused.
- [x] Wire the Phase 2 "Save as .mid" button to whatever `MidiClip` the
      interface currently holds.
  - It now also shows whether the save succeeded or failed, in the
    editor itself, not only in the log (a review after this phase
    flagged the log-only version as a real gap).
  - Note: the editor's clip (what "Generate" and "Save as .mid" use)
    and `AgentPlugin::clip` (what the audio thread plays back live,
    from Phase 2) are still two separate values. Phase 5 connects
    them; see section 5 for why that handoff needs more thought than a
    plain channel.
- [ ] Add the drag-out control from Phase 2 here, once that phase's
      platform-specific drag-and-drop work is done.
  - Still not done; see Phase 2.

#### Phase 3.5 — UI and background-task restructuring

A review after Phase 3 (see `REVIEW.md`) found two structural problems,
fixed here, ahead of Phase 4, since Phase 4 builds directly on both:

- [x] Move the UI's reusable state and rendering into `agent-ui`, which
      was an unused placeholder until now, even though `TODO.md` and
      `README.md` already described it as owning the plugin window.
  - `agent-ui` now has no dependency on `nih_plug`: it exposes
    `EditorState`, `GenerationStatus`, `UiAction`, and a `draw()`
    function that returns which action the user asked for.
  - `agent-plugin` still creates the actual `nih_plug_egui` editor,
    decides what an action means, and does host-integration work such
    as opening the save dialog.
- [x] Give the editor a real seam for background work, instead of
      calling the stub inline on the UI thread.
  - See `crates/agent-plugin/src/background.rs`: `GenerateTask`
    carries a request ID, not the prompt text; `GenerationStore` holds
    the actual prompts and results, behind a `Mutex`, shared between
    the editor and `Plugin::task_executor()`; and the editor polls for
    a result each frame while a request is pending.
  - This is real plumbing, even though the work it runs is still the
    Phase 3 stub. Phase 4 and Phase 5 replace `generate_stub()` with a
    real provider call; the rest should not need to change.
- [x] A generated clip's note list no longer grows the window without
      bound.
  - `agent_ui::draw()` puts it in a scrolling area with a fixed
    maximum height.

### Phase 4 — AI provider clients

This phase builds the provider clients, and the settings the user needs
to configure them. It does not wire them into the "Generate" button:
that button still uses the Phase 3 stub. Connecting a real provider
call to a real `MidiClip` is Phase 5's job, since it also needs a
prompt template and a JSON reply format, which do not exist yet.

- [x] Define an `AiProvider` trait with one method: send a prompt,
      return text.
  - See `crates/agent-core/src/provider.rs`.
  - Every provider call is blocking (synchronous), not `async`, so it
    can run inside a `nih_plug` background task with no separate async
    runtime; see section 5.
- [x] Implement the trait for OpenAI-compatible APIs. Support a
      configurable base URL, so the same code works for OpenAI,
      DeepSeek, OpenRouter, Ollama, and LM Studio.
  - See `OpenAiCompatibleProvider`.
  - Tested against a small, local, single-response HTTP server built
    with `std::net::TcpListener`, not a real network call: a
    successful reply, an error status, and a malformed response body.
- [x] Implement the trait for the Anthropic Claude API.
  - See `AnthropicProvider`.
  - Tested the same way as the OpenAI-compatible client.
- [x] Add a settings panel in the UI for provider choice, API key, and
      base URL.
  - See `agent_ui::ProviderSettings`, and the "Provider settings"
    section `agent_ui::draw()` adds.
  - Picking a different provider kind loads whatever key is already
    saved for it, instead of leaving the previous provider's key
    visible under the wrong one.
- [x] Store the API key with the `keyring` crate.
  - See `crates/agent-plugin/src/settings.rs`, the only place in the
    project that calls into `keyring`; `agent-ui` does not know the
    keychain exists.
  - Saving happens only when the user presses "Save API key", not on
    every keystroke.
  - A review found four real problems here, all fixed now:
    - Every OpenAI-compatible service shared one keychain entry, keyed
      only on provider kind. Switching between OpenAI, DeepSeek,
      OpenRouter, Ollama, and LM Studio lost whichever key was saved
      for the one used before. The keychain entry is now keyed on
      provider kind *and* base URL, so each distinct service keeps its
      own key.
    - Pressing "Save API key" with an empty field silently overwrote a
      previously saved, valid key with an empty one. Saving an empty
      or all-whitespace key is now rejected, with a message, instead of
      saved.
    - Every keyring failure, not only a genuinely missing key, was
      treated as "no key saved", which looks identical in the editor
      to a normal first-run state. A real failure (a locked keychain,
      no Secret Service running, and so on) is now shown to the user
      instead of hidden.
    - `kind`, `base_url`, and `model` were not persisted at all: a
      plugin restart reset them to `ProviderSettings::defaults_for`'s
      hard-coded starting point. Pressing "Save API key" now also
      writes these three non-secret fields to a small JSON file in the
      OS's normal config directory (`directories` crate), which is
      read back the next time the editor opens. `api_key` itself is
      never written to this file; it stays in the keychain.
  - Still not confirmed against a real macOS Keychain permission
    prompt, or against a real Linux Secret Service provider (for
    example GNOME Keyring or KWallet); see section 11.

### Phase 5 — Prompt-to-MIDI pipeline
- [x] Write a system prompt that asks the model for MIDI notes in a fixed
      JSON format.
  - See `SYSTEM_PROMPT` in `crates/agent-core/src/pipeline.rs`.
  - The JSON format uses beats (quarter notes), not ticks, for note
    timing: a model reasons about music in beats far more reliably
    than in an arbitrary tick resolution. `parse_clip_reply` converts
    beats to ticks, using `DEFAULT_TICKS_PER_QUARTER`.
- [x] Parse the JSON reply into a `MidiClip`.
  - See `parse_clip_reply()`. It tolerates small formatting slips a
    model commonly makes despite being told not to, such as a
    Markdown code fence around the JSON object, or text before or
    after it.
  - It reuses `MidiClip::validate()` (a new method, factored out of
    `to_smf_bytes()` so it can run without also building bytes) to
    decide whether the notes are usable, the same way every other
    source of a `MidiClip` in this project is checked.
- [x] Handle bad replies: retry once, then show an error to the user.
  - See `generate_clip()`. On a bad first reply, it sends a second
    request that includes the first reply and what was wrong with it,
    so the model has a real chance to correct itself, not just a blind
    repeat of the same prompt.
  - A failure to reach the provider at all (`ProviderError`, for
    example a bad API key) is not retried this way: it would usually
    fail the same way again immediately. See `generate_clip()`'s docs
    for the reasoning.
  - The error message shown to the user, and fed back to the model on
    retry, is decided by `agent_ui::draw()` and `agent-plugin`'s
    editor code, the same as every other status message in the
    editor.
- [x] Design a real-time-safe way to send a freshly generated `MidiClip`
      to the audio thread, for live playback.
  - Used a triple buffer (the `triple_buffer` crate), through a new
    `crates/agent-plugin/src/clip_publisher.rs` module, exactly as
    this file recommended. Writing a new clip (on the editor's or the
    background executor's thread) drops the old one there; reading
    the latest clip (on the audio thread, every `process()` call)
    never allocates and never drops a value.
  - Each published clip carries a generation number, so `process()`
    can tell a genuine clip replacement apart from an ordinary
    transport update, and clean up (send note-offs for anything still
    sounding from the old clip) and resynchronize (recompute the loop
    length from the new clip, and restart the playhead from the
    host's position, or from 0 if the host reports none) only when the
    clip itself actually changed.
  - The demo clip's loop length was a fixed constant (4 beats); a
    generated clip's is not the same for every clip, so
    `loop_length_ticks_for()` now computes it from the clip's own last
    note, every time the clip changes.
- [x] Test the full path: type a prompt, get real notes from a real
      provider, and confirm the result with the two ways this project
      actually moves a clip into a DAW: dragging or saving the clip
      (once Phase 2's drag-out control exists, or with "Save as .mid"
      until then), and, only as an optional check, routed live
      playback.
  - Automated tests cover every step of this path except the real
    network call itself, which already has its own tests in Phase 4
    (`agent-core`'s provider tests) against a local mock server: the
    system prompt and JSON parsing (`agent-core::pipeline`, 8
    tests), the retry behavior (mocked `AiProvider`, no real network
    call), the clip publisher's generation tracking
    (`clip_publisher`, 4 tests), and the background request-ID
    bookkeeping (`background`, 5 tests, one added in Phase 6; see
    below).
  - Typing a real prompt, with a real provider and a real API key, and
    checking the result by ear or by opening the saved `.mid` file in
    a DAW, has not been done: this needs a maintainer with an API key,
    and, for the "drag out" half of the acceptance test, still needs
    Phase 2's drag-and-drop work. Track this alongside the other
    manual, real-DAW checks in section 9.
  - An earlier version of this file used "see the notes in the host
    piano roll" as the acceptance test; a review after Phase 3 pointed
    out that this does not match how MIDI Agent, or this project's own
    Phase 2 design, actually moves notes into a DAW.

### Phase 6 — MIDI import and variation
- [x] Add a file picker or drag-and-drop target for `.mid` files.
  - Used a file picker only ("Load .mid..."), the same way "Save as
    .mid" already does, with `rfd::FileDialog::new().pick_file()`.
  - A drag-and-drop *target* (accepting a file dropped onto the
    plugin window) is a different, and easier, problem than Phase
    2's drag-*out* problem, since it only needs to receive an OS
    event, not start a native drag session. It was still left out of
    this phase: whether `nih_plug_egui`'s windowing backend
    (`egui-baseview`) forwards a native file-drop event into egui's
    `dropped_files` input at all is unconfirmed, and this project has
    no way to test that without a real host. The file picker already
    satisfies this item, since the plan said "a file picker **or**
    drag-and-drop target".
- [x] Read the file into a `MidiClip` with the Phase 1 code.
  - See `load_clip_from_file()` in `crates/agent-plugin/src/lib.rs`.
    It reads the chosen path's bytes, then calls
    `MidiClip::from_smf_bytes()`.
  - A review found this read the whole chosen file with no size limit
    at all, on the UI thread, so an accidental or hostile
    multi-gigabyte file could stall the editor and exhaust memory. It
    now checks the file's size first, and also bounds the read itself
    (the same defense-in-depth shape `agent_core::provider` already
    uses for a provider response body), and rejects anything over 5 MB
    before `MidiClip::from_smf_bytes()` ever sees it. Moving the read
    itself onto a background thread, so a large-but-allowed file
    cannot stall the UI either, is not done yet; it needs a seam like
    `background::GenerationStore`'s, but for a file load instead of a
    provider call. Track this alongside the other resource-limit
    follow-ups this review found.
- [x] Add a prompt mode that sends the existing notes plus a text
      instruction, such as "add a harmony line" or "make a variation".
  - Added a second button, "Vary current clip", next to "Generate".
    It sends the loaded clip's notes, as the same beats-based JSON
    `parse_clip_reply` reads, plus the prompt box's text as the
    instruction. See `agent_core::generate_variation()`.
  - It reuses `generate_clip()`'s retry behavior directly, by building
    a single, richer prompt string that embeds both the existing clip
    and the instruction: the retry logic does not need to know the
    difference between "generate something new" and "vary this clip".
  - Rejects an invalid existing clip (one `MidiClip::validate()` would
    reject) before ever contacting the provider, with a dedicated
    `PipelineError::InvalidInput`, and rejects an empty instruction in
    the editor before submitting a request at all, the same way the
    Phase 3 stub once rejected an empty prompt.
  - Both "Load .mid..." and "Vary current clip" run through the same
    background request system Phase 3.5 and Phase 5 built
    (`background::GenerationStore`), extended with an optional
    existing clip on a request, rather than a separate system.
- [x] Return the new `MidiClip` and show it the same way as Phase 5.
  - A loaded file, and a variation result, both replace
    `EditorState::clip` (for "Save as .mid" and display) and publish
    to the audio thread through `clip_publisher` (for live playback),
    exactly like a fresh "Generate" result does.

### Phase 7 — Packaging
- [x] Write a build script that copies the compiled plugin into the
      correct bundle format for CLAP and VST3.
  - Done ahead of schedule, during Phase 0: the `xtask` crate does
    this with the `nih_plug` bundler, run through
    `cargo xtask bundle agent-plugin --release`, or `make pack`.
- [x] Write install steps for Linux: copy files to `~/.clap` and
      `~/.vst3`.
  - Done ahead of schedule: see `make install` and `README.md`.
- [x] Write install steps for macOS: build a `.vst3` and `.clap` bundle
      with the correct `Info.plist`.
  - Done ahead of schedule: the `nih_plug` bundler writes a correct
    `Info.plist`, and adds an ad-hoc code signature.
  - `make install` copies the bundles to `~/Library/Audio/Plug-Ins/`.
- [ ] Sign the macOS bundles with a real Apple Developer ID, instead of
      an ad-hoc signature.
  - This needs a paid Apple Developer account, so it stays open until
    a maintainer has one.
- [x] Write a short install guide in `README.md`.
  - Done ahead of schedule, alongside the `Makefile`.

### Phase 8 — Audio-to-MIDI transcription (stretch goal)
- [ ] Research an open, license-free pitch detection model. Basic Pitch by
      Spotify is one option, if its license allows this use.
- [ ] Run the model with the `ort` crate (ONNX Runtime bindings), so no
      Python dependency is needed at run time.
- [ ] Add an audio file import step in the UI.
- [ ] Convert model output into a `MidiClip` with the Phase 1 code.

### Phase 9 — Extra AI providers (stretch goal)
- [ ] Add Google Gemini support.
- [ ] Add xAI Grok support.
- [ ] Add a generic OpenRouter mode that lists many models.

### Phase 10 — AU and AAX (stretch goal, may not happen)
- [ ] Check the state of Rust AU tooling again. Add AU support only if a
      solid Rust option exists.
- [ ] Do not add AAX unless a project maintainer accepts the Avid license
      terms.

## 9. Testing plan

- [x] Add unit tests for the MIDI conversion code in `agent-core`.
  - Done in Phase 1, and expanded after a Phase 3 review found real
    gaps in the import side.
  - The tests now cover normal round trips, and failure modes:
    out-of-range fields, silent and zero-duration notes, overlapping
    notes, bad time signatures and tempo (including a tempo too slow
    to fit the file format's tempo field, not only non-finite or
    negative tempo), malformed bytes, gaps too large to encode,
    same-tick retriggers, multi-track files, a zero
    ticks-per-quarter-note value, a malformed time signature
    denominator, format 2 (sequential) files, and a per-track tick
    overflow.
- [x] Add unit tests for the live MIDI scheduler in `agent-plugin`.
  - Not in the original plan; added after a Phase 3 review found real
    scheduler bugs (see Phase 2).
  - The scheduling math lives in `crates/agent-plugin/src/scheduler.rs`,
    with no `nih_plug` types in it, specifically so it can have plain
    unit tests with no host needed.
  - Tests cover: a single note's on and off, a note left open across
    two calls, the loop-boundary bug's regression case, starting
    playback in the middle of a loop, note-off/note-on ordering at the
    same sample, cleanup note-offs for stuck notes, and, added after
    the memory-leak bug described in Phase 2 above, five consecutive
    calls each spanning exactly one full loop, confirming none of them
    hangs or emits more than one on/off pair per note.
  - Detecting a transport discontinuity itself is not unit tested,
    since that needs a real or mocked `nih_plug` `Transport`; it is
    only exercised indirectly, by `clap-validator`'s `transport-fuzz`
    tests.
- [x] Add unit tests for the AI provider clients in `agent-core`.
  - Not in the original plan for this section; added in Phase 4.
  - Both clients are tested against a small local HTTP server,
    covering a successful reply, a non-success status code, and a
    malformed response body, with no real network call and no real
    API key needed.
- [x] Add unit tests for the JSON parsing code in the prompt pipeline.
  - Added in Phase 5. See `crates/agent-core/src/pipeline.rs`.
  - Tests cover: a well-formed reply, a reply wrapped in a Markdown
    code fence and surrounding text, a reply with no JSON object, a
    reply with no notes, and a reply whose notes fail
    `MidiClip::validate()`.
  - The retry behavior (`generate_clip()`) is tested with a scripted
    `AiProvider` test double that returns canned replies and counts
    how many times it was called, confirming: a good first reply
    needs no retry, a bad first reply gets exactly one retry, two bad
    replies in a row fail with both attempts described, and a
    provider-level error (not a bad reply) is not retried at all.
- [x] Add unit tests for the "vary current clip" pipeline.
  - Added in Phase 6, alongside `generate_variation()`.
  - The scripted `AiProvider` test double from the tests above was
    extended to also record every prompt it was sent, so a test can
    confirm the existing clip's notes, and the instruction, both
    actually reach the model.
  - Tests cover: a clip serialized to JSON and read back by
    `parse_clip_reply` round-trips its notes, the existing clip and
    instruction both appear in the prompt sent to the provider, the
    same retry behavior as `generate_clip()` applies, and an invalid
    existing clip is rejected before the provider is ever contacted
    (confirmed by asserting the scripted provider's call count is 0).
- [x] Add unit tests for the real-time-safe clip handoff to the audio
      thread.
  - Not in the original plan; added in Phase 5, alongside the handoff
    itself. See `crates/agent-plugin/src/clip_publisher.rs`.
  - Tests cover: the initial clip is readable at generation 0, a
    published update is readable with a new generation, generation
    numbers keep increasing across several publishes, and a cloned
    publisher handle shares the same buffer as the original.
- [x] Add unit tests for the background request-ID bookkeeping.
  - Not in the original plan; added in Phase 3.5, expanded in Phase 5
    and Phase 6. See `crates/agent-plugin/src/background.rs`.
  - Tests cover only the bookkeeping (assigning IDs, a result being
    delivered by `poll()` exactly once, and a request correctly
    recording an existing clip for a "vary current clip" request),
    not a real network call: `run()`'s own behavior is already
    covered by `agent_core::pipeline`'s and `agent_core::provider`'s
    tests.
- [ ] Add a manual test checklist. Run it before each release:
  - [ ] Plugin loads in a Linux host.
  - [ ] Plugin loads in a macOS host.
  - [ ] Prompt generates correct notes with a live API key.
  - [ ] Prompt generates correct notes with a local model (Ollama).
  - [ ] MIDI import and variation works.
  - [ ] API key is not visible in log output.

## 10. License

- [x] Choose a permissive license for the project.
  - The project uses a dual license: MIT or Apache-2.0, at the user's
    choice. This is the common choice for Rust projects.
  - `Cargo.toml` declares it, and the full text is in `LICENSE-MIT` and
    `LICENSE-APACHE`.
- [ ] Check the license of every dependency before release. Confirm each
      one allows commercial and open source use.
- [ ] Add clear text to `README.md`: users own the MIDI files they
      generate. The project places no restriction on generated MIDI.

## 11. Open questions

- [ ] Which local model runtime should the quick-start guide use: Ollama,
      LM Studio, or both?
- [ ] Should the plugin ship a small, built-in fallback model for offline
      use, or always require the user to set up a provider?
- [ ] `nih_plug`'s built-in state loader can try a very large memory
      allocation when it reads corrupted or random state bytes.
  - This can abort the process. `clap-validator` found this with its
    `state-invalid-random` test.
  - Track the upstream `nih_plug` issue tracker for a fix, or add a
    size check before Phase 5, when the plugin starts to save real
    state (API keys, provider choice, and so on).
- [ ] `clap-validator`'s `param-conversions` test divides by zero when a
      plugin has no parameters.
  - This is a bug in `clap-validator`, not in our plugin.
  - Re-run this test once the plugin has real parameters, in a later
    phase, to confirm it passes then.
- [ ] "Save as .mid" (`rfd`, added in Phase 2) has not been confirmed on
      a real Linux desktop.
  - With `rfd`'s default features, it needs an XDG desktop portal
    backend running (for example `xdg-desktop-portal-gtk`), which most
    desktop environments already include, but this project has not
    tested that yet.
  - Do this once a Linux machine is available, alongside the Phase 0
    DAW check.
- [ ] The API key storage added in Phase 4 (`keyring`) has not been
      confirmed against a real permission prompt.
  - Reading a key that does not exist yet was tested (it just returns
    nothing, with no prompt), but this project has not tested saving a
    real key on macOS, where an ad-hoc-signed development build's code
    signature can change on every rebuild, which can make the OS treat
    it as a "different app" and re-prompt for Keychain access on every
    build.
  - This also has not been tested against a real Linux Secret Service
    provider (for example GNOME Keyring or KWallet).
  - Do this once a maintainer can test both by hand; consider a proper
    Developer ID signature for macOS before release, so Keychain
    access stays stable across builds.
