# Standard Makefile for this project.
#
# This file follows the "standard makefile" convention from:
# https://www.crisjr.eng.br/notes/standard_makefile.html
#
# The convention has ten steps: build, bootstrap, test, run, pack,
# install, save, load, clean, and teardown. Each step is a phony target,
# so it always runs, even if a file with the same name exists.
#
# This project skips "save" and "load". Those two steps package the
# application as a Docker image. A native CLAP/VST3 plugin runs inside a
# host DAW on the developer's own OS, so a Docker image does not help
# here.
#
# Run `make` with no target to run the default step, which is `build`.

PLUGIN := agent-plugin
BUNDLE_DIR := target/bundled
CLAP_BUNDLE := $(BUNDLE_DIR)/$(PLUGIN).clap
VST3_BUNDLE := $(BUNDLE_DIR)/$(PLUGIN).vst3

DEV_TOOLS_DIR := .dev-tools
CLAP_VALIDATOR_REPO := https://github.com/free-audio/clap-validator.git
CLAP_VALIDATOR := $(DEV_TOOLS_DIR)/clap-validator/target/release/clap-validator

UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
PLUGINVAL := /Applications/pluginval.app/Contents/MacOS/pluginval
else
PLUGINVAL := pluginval
endif

.PHONY: default
default: build

# "build" compiles the workspace. This is the default step.
.PHONY: build
build:
	cargo build --workspace

# "bootstrap" sets up temporary tools for development. It does not
# change the plugin code.
.PHONY: bootstrap
bootstrap: $(CLAP_VALIDATOR)
ifeq ($(UNAME_S),Darwin)
	@echo "bootstrap: installing pluginval with Homebrew"
	@test -x "$(PLUGINVAL)" || brew install --cask pluginval
else ifeq ($(UNAME_S),Linux)
	@echo "bootstrap: installing Linux build libraries with apt-get"
	sudo apt-get update
	sudo apt-get install -y libasound2-dev libjack-jackd2-dev libxcb1-dev \
	  libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev \
	  libxcb-shape0-dev libxcb-xfixes0-dev
	@echo "bootstrap: pluginval has no Linux package."
	@echo "bootstrap: get it by hand from https://github.com/Tracktion/pluginval/releases"
else
	@echo "bootstrap: unknown OS '$(UNAME_S)'. Install build tools by hand. See README.md."
endif

# This file target builds clap-validator once, and skips the build on
# later runs, because the file already exists.
$(CLAP_VALIDATOR):
	@echo "bootstrap: building clap-validator from source"
	mkdir -p $(DEV_TOOLS_DIR)
	git clone --depth 1 $(CLAP_VALIDATOR_REPO) $(DEV_TOOLS_DIR)/clap-validator
	cd $(DEV_TOOLS_DIR)/clap-validator && cargo build --release

# "test" runs the automated unit tests.
.PHONY: test
test:
	cargo test --workspace

# "run" executes the plugin in a quick way, for manual testing. This
# project has no standalone application yet, so this step runs the
# plugin bundles through headless validator tools instead. Update this
# step once a standalone build exists.
.PHONY: run
run: pack
	@echo "run: checking the VST3 bundle with pluginval"
	$(PLUGINVAL) --strictness-level 5 --validate $(VST3_BUNDLE)
	@echo "run: checking the CLAP bundle with clap-validator"
	$(CLAP_VALIDATOR) validate $(CLAP_BUNDLE)

# "pack" creates the distributable plugin bundles.
.PHONY: pack
pack: build
	cargo xtask bundle $(PLUGIN) --release

# "install" copies the plugin bundles to the local plugin folders, so a
# DAW on this machine can find them.
.PHONY: install
install: pack
ifeq ($(UNAME_S),Darwin)
	mkdir -p ~/Library/Audio/Plug-Ins/CLAP ~/Library/Audio/Plug-Ins/VST3
	rm -rf ~/Library/Audio/Plug-Ins/CLAP/$(PLUGIN).clap
	rm -rf ~/Library/Audio/Plug-Ins/VST3/$(PLUGIN).vst3
	cp -r $(CLAP_BUNDLE) ~/Library/Audio/Plug-Ins/CLAP/
	cp -r $(VST3_BUNDLE) ~/Library/Audio/Plug-Ins/VST3/
else ifeq ($(UNAME_S),Linux)
	mkdir -p ~/.clap ~/.vst3
	rm -rf ~/.clap/$(PLUGIN).clap
	rm -rf ~/.vst3/$(PLUGIN).vst3
	cp -r $(CLAP_BUNDLE) ~/.clap/
	cp -r $(VST3_BUNDLE) ~/.vst3/
else
	@echo "install: unknown OS '$(UNAME_S)'. Copy the bundles from $(BUNDLE_DIR) by hand."
endif

# "clean" removes build artifacts. It runs `cargo clean`, which deletes
# the `target` directory. It does not touch source files, so it is safe
# to run at any time.
.PHONY: clean
clean:
	cargo clean

# "teardown" removes the tools that "bootstrap" set up.
.PHONY: teardown
teardown:
	rm -rf $(DEV_TOOLS_DIR)
ifeq ($(UNAME_S),Darwin)
	@echo "teardown: removing pluginval"
	-brew uninstall --cask pluginval
endif
