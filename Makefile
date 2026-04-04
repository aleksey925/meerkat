VER ?= 0.0.0-dev
TAURI_VERSION_CONFIG := --config '{"version": "$(VER)"}'

deps:
	npm i

dev:
	npm run tauri dev

build:
	npm run tauri build -- $(TAURI_VERSION_CONFIG)
