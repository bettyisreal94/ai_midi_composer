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
- [x] Write unit tests for the conversion functions. Tests cover: a
      round trip of notes, a round trip of tempo and time signature, an
      empty clip, and rejecting a note with an out-of-range pitch.

### Phase 2 — Plugin skeleton with fixed MIDI output
- [ ] Build a CLAP/VST3 plugin that sends one fixed test note pattern to
      the host, as a MIDI effect.
- [ ] Confirm the host piano roll shows the correct notes.
- [ ] Confirm this works on Linux and on macOS.

### Phase 3 — Basic user interface
- [ ] Add a text box for the prompt.
- [ ] Add a "Generate" button.
- [ ] Add a status label for "working", "done", and "error" states.
- [ ] Wire the button to a stub function. The stub returns a fixed
      `MidiClip` for now.

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
- [ ] Write a build script that copies the compiled plugin into the
      correct bundle format for CLAP and VST3.
- [ ] Write install steps for Linux: copy files to
      `~/.clap` and `~/.vst3`.
- [ ] Write install steps for macOS: build a `.vst3` and `.clap` bundle
      with the correct `Info.plist`, and sign the code with an Apple
      Developer ID if the user has one.
- [ ] Write a short install guide in `README.md`.

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

- [ ] Add unit tests for the MIDI conversion code in `agent-core`.
- [ ] Add unit tests for the JSON parsing code in the prompt pipeline.
- [ ] Add a manual test checklist. Run it before each release:
  - [ ] Plugin loads in a Linux host.
  - [ ] Plugin loads in a macOS host.
  - [ ] Prompt generates correct notes with a live API key.
  - [ ] Prompt generates correct notes with a local model (Ollama).
  - [ ] MIDI import and variation works.
  - [ ] API key is not visible in log output.

## 10. License

- [ ] Choose a permissive license for the project, such as MIT or
      Apache-2.0.
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
