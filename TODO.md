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

Version 1 must have these features:

- [ ] Text-to-MIDI generation. The user types a prompt. The plugin returns
      MIDI notes.
- [ ] MIDI file import. The user loads a MIDI file. The plugin can extend it
      or create a variation of it.
- [ ] Support for OpenAI-compatible chat APIs. This covers OpenAI, DeepSeek,
      OpenRouter, Ollama, and LM Studio, because they use the same API
      shape.
- [ ] Support for the Anthropic Claude API.
- [ ] A user interface inside the plugin window. The interface has a text
      box, a generate button, and a provider settings panel.
- [ ] Plugin formats: CLAP and VST3.
- [ ] Platforms: Linux and macOS.
- [ ] Local storage of API keys. Keys must not appear in plain log files.

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

The plugin runs network requests on a background thread. The audio thread
must never wait for a network reply. The audio thread reads results from a
lock-free queue.

A review of phases 0 and 1 (see `REVIEW.md`) asked for this to be more
specific, before Phase 4 and Phase 5 write real code here. This is the
intended design, to check against when that code is written:

- `agent-plugin` owns a small piece of shared state: the current prompt
  text, the current status ("idle", "working", "done", "error"), and
  the most recent generated `MidiClip`. `nih_plug`'s `editor()` method
  builds the `agent-ui` window from a clone of a handle to this state
  (for example an `Arc<Mutex<...>>`, or a similar type built for this
  purpose), so the UI thread can read and write it directly.
- `nih_plug` has a built-in mechanism for exactly this kind of work: the
  `Plugin::BackgroundTask` associated type, together with
  `ProcessContext::execute_background()` and the plugin's task executor
  closure. Phase 4 and Phase 5 should use this mechanism to run AI
  provider network calls on a host-managed background thread, instead
  of adding a separate `tokio` runtime by hand. The `BackgroundTask`
  placeholder type in the current `agent-plugin` skeleton
  (`type BackgroundTask = ();`) is there for this: later phases replace
  `()` with a real task type, such as an enum with a "generate from
  this prompt" variant.
- The audio thread must not lock a `Mutex` that the network code might
  also hold for long. Use a small, bounded, non-blocking channel to
  hand a finished `MidiClip` from the background task back to the
  audio-adjacent state (for example a bounded `crossbeam-channel`, read
  with `try_recv`, which never blocks the audio thread even if empty).
  Do not read this channel from the UI thread directly; let the UI
  thread read the shared state that the audio/background side updates
  after it drains the channel.

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
- `nih_plug_egui` — draws the plugin window with `egui`.
- `midly` — reads and writes standard MIDI files.
- `reqwest` — sends HTTP requests to AI providers.
- `tokio` — runs the background network thread.
- `serde` and `serde_json` — reads and writes JSON.
- `crossbeam-channel` — sends data between the audio thread and the
  network thread.
- `keyring` — stores API keys in the OS secure storage (Keychain on
  macOS, Secret Service on Linux).
- `directories` — finds the correct config folder on each OS.

## 7. Repository layout

```
vst/
  Cargo.toml               # workspace file
  crates/
    agent-core/             # MIDI model, AI clients, prompt logic
    agent-plugin/           # CLAP/VST3 plugin, uses nih_plug
    agent-ui/               # egui interface code
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
  - [x] `pluginval` (strictness 5) on the VST3 bundle. Result: SUCCESS.
  - [x] `clap-validator` on the CLAP bundle. Result: 30 passed, 2 failed.
        Both failures are tracked in section 11 (Open questions). Neither
        one blocks later phases.
- [ ] Load the plugin in a real DAW (Reaper or Bitwig) on Linux and on
      macOS. Do this once a DAW is available on a test machine.
- [ ] Set up CI to build the workspace on Linux and macOS. Postponed. Do
      this later, once the project has a git remote and the core features
      are more stable.

### Phase 1 — MIDI data model
- [x] Define a `Note` struct: pitch, velocity, start time, duration,
      channel. See `crates/agent-core/src/midi.rs`.
- [x] Define a `MidiClip` struct: a list of notes plus tempo and time
      signature.
- [x] Write functions to convert a `MidiClip` to a standard MIDI file, and
      back, using `midly`. These are `MidiClip::to_smf_bytes` and
      `MidiClip::from_smf_bytes`. They build a format 0 (single track)
      file. Reading back merges notes from every track in a file, and
      keeps the first tempo and time signature found.
- [x] Write unit tests for the conversion functions. See section 9
      (Testing plan) for the full list of what they cover.
- [x] Harden the model against silent data loss and invalid files. A
      review of this phase found that the first version accepted
      silent (velocity 0) and zero-duration notes, encoded bad time
      signatures without complaint, and could lose notes when two
      notes of the same channel and pitch overlapped in time. The
      current version rejects all of these on export, with a specific
      `MidiError` for each case, and uses a documented, deterministic
      policy (last-in-first-out matching) for overlapping notes found
      when reading a file written by another program.

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

- [ ] Decide, and record here, the exact VST3 category for this plugin
      (for example `Vst3SubCategory::Instrument`), and the CLAP category
      (`ClapFeature::NoteEffect` or `ClapFeature::Instrument`). Keep the
      dummy stereo audio bus.
- [ ] Build a small, fixed `MidiClip` value inside the plugin, using the
      Phase 1 data model. This stands in for real AI generation, until
      Phase 5.
- [ ] Add a "drag out" control in the plugin window: the user can drag
      the generated clip from the plugin into a DAW track. This is the
      main way to get notes out of the plugin, so treat it as required
      for version 1, not a stretch goal.
- [ ] Add a "Save as .mid" button, so the user can export the clip as a
      file, for DAWs or workflows that do not support dragging a clip
      out of a plugin window.
- [ ] As a secondary feature, also send the clip as live MIDI output
      events, for hosts that support live MIDI from a plugin. Test this
      on a small, explicit list of hosts, since host support for this
      varies far more than support for audio effects does.
- [ ] Confirm this works on Linux and on macOS.

### Phase 3 — Basic user interface
- [ ] Add a text box for the prompt.
- [ ] Add a "Generate" button.
- [ ] Add a status label for "working", "done", and "error" states.
- [ ] Wire the button to a stub function. The stub returns a fixed
      `MidiClip` for now.
- [ ] Wire the Phase 2 drag-out control and "Save as .mid" button to
      whatever `MidiClip` the interface currently holds, so a later
      generated clip replaces the Phase 2 placeholder clip without
      further plumbing work.

### Phase 4 — AI provider clients
- [ ] Define an `AiProvider` trait with one method: send a prompt, return
      text.
- [ ] Implement the trait for OpenAI-compatible APIs. Support a
      configurable base URL, so the same code works for OpenAI, DeepSeek,
      OpenRouter, Ollama, and LM Studio.
- [ ] Implement the trait for the Anthropic Claude API.
- [ ] Add a settings panel in the UI for provider choice, API key, and
      base URL.
- [ ] Store the API key with the `keyring` crate.

### Phase 5 — Prompt-to-MIDI pipeline
- [ ] Write a system prompt that asks the model for MIDI notes in a fixed
      JSON format.
- [ ] Parse the JSON reply into a `MidiClip`.
- [ ] Handle bad replies: retry once, then show an error to the user.
- [ ] Send the finished `MidiClip` to the audio thread through the
      lock-free queue.
- [ ] Test the full path: type a prompt, get real notes from a real
      provider, see the notes in the host piano roll.

### Phase 6 — MIDI import and variation
- [ ] Add a file picker or drag-and-drop target for `.mid` files.
- [ ] Read the file into a `MidiClip` with the Phase 1 code.
- [ ] Add a prompt mode that sends the existing notes plus a text
      instruction, such as "add a harmony line" or "make a variation".
- [ ] Return the new `MidiClip` and show it the same way as Phase 5.

### Phase 7 — Packaging
- [x] Write a build script that copies the compiled plugin into the
      correct bundle format for CLAP and VST3. Done ahead of schedule,
      during Phase 0: the `xtask` crate does this with the `nih_plug`
      bundler, run through `cargo xtask bundle agent-plugin --release`,
      or `make pack`.
- [x] Write install steps for Linux: copy files to `~/.clap` and
      `~/.vst3`. Done ahead of schedule: see `make install` and
      `README.md`.
- [x] Write install steps for macOS: build a `.vst3` and `.clap` bundle
      with the correct `Info.plist`. Done ahead of schedule: the
      `nih_plug` bundler writes a correct `Info.plist`, and adds an
      ad-hoc code signature. `make install` copies the bundles to
      `~/Library/Audio/Plug-Ins/`.
- [ ] Sign the macOS bundles with a real Apple Developer ID, instead of
      an ad-hoc signature. This needs a paid Apple Developer account, so
      it stays open until a maintainer has one.
- [x] Write a short install guide in `README.md`. Done ahead of
      schedule, alongside the `Makefile`.

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

- [x] Add unit tests for the MIDI conversion code in `agent-core`. Done
      in Phase 1. The tests cover normal round trips, and failure modes:
      out-of-range fields, silent and zero-duration notes, overlapping
      notes, bad time signatures and tempo, malformed bytes, gaps too
      large to encode, same-tick retriggers, and multi-track files.
- [ ] Add unit tests for the JSON parsing code in the prompt pipeline.
- [ ] Add a manual test checklist. Run it before each release:
  - [ ] Plugin loads in a Linux host.
  - [ ] Plugin loads in a macOS host.
  - [ ] Prompt generates correct notes with a live API key.
  - [ ] Prompt generates correct notes with a local model (Ollama).
  - [ ] MIDI import and variation works.
  - [ ] API key is not visible in log output.

## 10. License

- [x] Choose a permissive license for the project. The project uses a
      dual license: MIT or Apache-2.0, at the user's choice. This is the
      common choice for Rust projects. `Cargo.toml` declares it, and the
      full text is in `LICENSE-MIT` and `LICENSE-APACHE`.
- [ ] Check the license of every dependency before release. Confirm each
      one allows commercial and open source use.
- [ ] Add clear text to `README.md`: users own the MIDI files they
      generate. The project places no restriction on generated MIDI.

## 11. Open questions

- [ ] Which local model runtime should the quick-start guide use: Ollama,
      LM Studio, or both?
- [ ] Should the plugin ship a small, built-in fallback model for offline
      use, or always require the user to set up a provider?
- [ ] What is the exact JSON schema for MIDI notes sent by the AI model?
      Draft it early in Phase 5, since many parts of the code depend on
      it.
- [ ] `nih_plug`'s built-in state loader can try a very large memory
      allocation when it reads corrupted or random state bytes. This can
      abort the process. `clap-validator` found this with its
      `state-invalid-random` test. Track the upstream `nih_plug` issue
      tracker for a fix, or add a size check before Phase 5, when the
      plugin starts to save real state (API keys, provider choice, and
      so on).
- [ ] `clap-validator`'s `param-conversions` test divides by zero when a
      plugin has no parameters. This is a bug in `clap-validator`, not in
      our plugin. Re-run this test once the plugin has real parameters,
      in a later phase, to confirm it passes then.
