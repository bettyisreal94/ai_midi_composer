# AI MIDI Agent

> Status: work in progress. The plugin does not generate MIDI yet. See
> `TODO.md` for the full plan.

## What this project is

This project builds an open source plugin. The plugin will create MIDI data from text prompts. The plugin will use AI language models to do this. The plugin is written in Rust.

Read `TODO.md` for the full feature plan and the build phases.

## Supported platforms

- Linux
- macOS

Windows is not a target for this project right now.

## Plugin formats

The plugin builds as a CLAP plugin and as a VST3 plugin. AU and AAX are not part of this project yet. See `TODO.md`, section 4, for the reasons.

## Repository layout

```
vst/
  Cargo.toml          # workspace file
  crates/
    agent-core/        # MIDI data model and AI provider clients
    agent-plugin/       # the CLAP/VST3 plugin, built with nih_plug
    agent-ui/           # the plugin window (not built yet)
  xtask/                # build script that creates the plugin bundles
  TODO.md               # the project plan
```

## Quick start with `make`

This project has a `Makefile` with eight of the ten standard steps. Read `Makefile` for the exact commands. Every step is safe to run more than once.

| Step | What it does |
| --- | --- |
| `make` or `make build` | Compiles the workspace. This is the default step. |
| `make bootstrap` | Installs temporary dev tools: Linux build libraries, or `pluginval` on macOS, and builds `clap-validator` into `.dev-tools/`. |
| `make test` | Runs the automated unit tests. |
| `make run` | Builds the plugin bundles, then checks them with `pluginval` and `clap-validator`. There is no standalone application yet, so this is the closest thing to "running" the plugin. |
| `make pack` | Builds the CLAP and VST3 bundles, under `target/bundled/`. |
| `make install` | Copies the bundles to your local DAW plugin folders. |
| `make clean` | Runs `cargo clean`. Removes the `target` directory. Safe: it never touches source files. |
| `make teardown` | Removes the tools that `make bootstrap` installed. On macOS, this uninstalls `pluginval`. |

Run `make bootstrap` once, before your first build. Run `make pack` or `make install` for everyday plugin testing.

## Requirements

You need these tools to build the plugin:

- The Rust toolchain, version 1.98 or newer. Install it with [rustup](https://rustup.rs/).
- A C compiler. On macOS, install the Xcode Command Line Tools with `xcode-select --install`. On Linux, install `gcc` or `clang` with your package manager.

On Linux, you also need these development packages. On Debian and Ubuntu, install them with:

```sh
sudo apt-get install libasound2-dev libjack-jackd2-dev libxcb1-dev \
  libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libgtk-3-dev
```

`libgtk-3-dev` is for the "Save as .mid" file dialog, added in Phase 2. It comes from the `rfd` crate's default Linux file dialog backend.

Other Linux distributions need the same libraries, under different package names.

## Build the code

`make build` runs the commands in this section for you.

Run this command to check that the code compiles:

```sh
cargo check --workspace
```

Run this command to run the automated tests:

```sh
cargo test --workspace
```

## Build the plugin bundles

`make pack` runs the command in this section for you.

The plugin does not build as a normal Rust binary. It builds as a `cdylib`, and then a small tool packs it into a CLAP bundle and a VST3 bundle. This project uses the `nih_plug` bundler for this step, through the `xtask` crate.

Run this command to build both bundles:

```sh
cargo xtask bundle agent-plugin --release
```

The bundles appear here:

```
target/bundled/agent-plugin.clap
target/bundled/agent-plugin.vst3
```

## Install the plugin bundles for local testing

`make install` runs the commands in this section for you.

Copy the bundles into your host's plugin folders.

On Linux:

```sh
mkdir -p ~/.clap ~/.vst3
cp -r target/bundled/agent-plugin.clap ~/.clap/
cp -r target/bundled/agent-plugin.vst3 ~/.vst3/
```

On macOS:

```sh
mkdir -p ~/Library/Audio/Plug-Ins/CLAP ~/Library/Audio/Plug-Ins/VST3
cp -r target/bundled/agent-plugin.clap ~/Library/Audio/Plug-Ins/CLAP/
cp -r target/bundled/agent-plugin.vst3 ~/Library/Audio/Plug-Ins/VST3/
```

Then open your DAW and rescan its plugin list. The plugin shows up as "AI MIDI Agent (dev)".

## Test the plugin without a DAW

`make run` runs the commands in this section for you. `make bootstrap` installs both tools first.

You can check the plugin bundles with two free command-line tools. This is useful when no DAW is on the build machine.

- [`pluginval`](https://github.com/Tracktion/pluginval) checks the VST3
  bundle:

  ```sh
  pluginval --strictness-level 5 --validate target/bundled/agent-plugin.vst3
  ```

- [`clap-validator`](https://github.com/free-audio/clap-validator) checks
  the CLAP bundle:

  ```sh
  clap-validator validate target/bundled/agent-plugin.clap
  ```

Known result at the current stage: `clap-validator` reports 2 failed tests out of 32. Both are explained in `TODO.md`, section 11. Neither one blocks development.

## Format your code before you commit

Run this command before every commit:

```sh
cargo fmt --all
```

Check formatting without changing files:

```sh
cargo fmt --all -- --check
```

## Contributing

Read `TODO.md` first. It has the full plan, split into phases. Pick an unchecked item, and open a pull request when it is done.

## License

This project uses a dual license: MIT or Apache-2.0, at your choice. See `LICENSE-MIT` and `LICENSE-APACHE` for the full text, and `TODO.md`, section 10, for what is still open (checking dependency licenses, and a clear statement that users own their generated MIDI files).

