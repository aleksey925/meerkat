VER ?= 0.0.0
COMMIT_HASH := $(shell git rev-parse --short HEAD)
TAURI_VERSION_CONFIG := --config '{"version": "$(VER)", "bundle": {"macOS": {"bundleVersion": "$(COMMIT_HASH)"}}}'
export COMMIT_HASH

deps:
	npm i

dev:
	npm run tauri dev -- $(TAURI_VERSION_CONFIG)

build:
	npm run tauri build -- $(TAURI_VERSION_CONFIG)

test:
	cargo test --manifest-path src-tauri/Cargo.toml --lib
