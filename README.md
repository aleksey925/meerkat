# Meerkat

A macOS menubar app for tracking GitLab merge requests.

Meerkat lives in your system tray, periodically polls GitLab for MRs where
you are a reviewer or assignee, and sends notifications when something
changes.

- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
- [Development](#development)

## Features

- System tray icon with unread MR badge
- Native macOS notifications with sound and click-to-open for new review
  requests, review re-requests, MR updates, and failed pipelines
- Read/unread tracking that ignores your own actions; marking an MR read by
  hand can be flipped back to unread by a new re-request or a fresh comment
  from someone else
- Periodic background polling with configurable interval
- MR filtering by project and role (reviewer / assignee)
- Custom reminders for individual merge requests
- Detail panel with activity timeline
- Hides to tray on window close
- Light and dark theme (follows system)

## Installation

> **Requirements:** Meerkat ships only as an Apple Silicon (arm64) build and
> requires macOS Big Sur (11) or newer. Intel Macs are not supported - the
> downloaded DMG will not run on them.

The easiest way is via [Homebrew](https://brew.sh):

```bash
brew install --cask aleksey925/apps/meerkat
```

> **Note:** Meerkat is not signed with an Apple Developer certificate, so macOS
> Gatekeeper would normally block it. The cask handles this for you: during
> install it runs `xattr -dr com.apple.quarantine` on the app to strip the
> quarantine flag, so no extra steps are needed.

Alternatively, download the latest release from
[releases](https://github.com/aleksey925/meerkat/releases) and install it
manually.

> **Note:** With a manual install, Gatekeeper will warn that the app is
> damaged or can't be opened (same unsigned-build reason as above). To fix
> this, run:
>
> ```bash
> xattr -dr com.apple.quarantine /Applications/Meerkat.app
> ```

Also, you can [build from source](#build).

## Usage

1. Open **Settings** (the gear in the sidebar, or `Cmd+,`).
2. Enter your **GitLab URL** (for example `https://gitlab.example.com`).
3. Enter a **personal access token** with the `read_api` (or `api`) scope.
   The token is stored in your OS keychain (service `meerkat`, account
   `gitlab-pat`), never in a plain file.
4. Click **Save**. The status badge in the Connection card turns
   **Connected** on success; use **Disconnect** to stop fetching updates and
   remove the token from the OS keychain (your URL stays filled in and the
   account's data is kept for when you reconnect).

## Development

### Prerequisites

- macOS
- [mise](https://mise.jdx.dev/getting-started.html#installing-mise-cli) for managing toolchains
- Xcode Command Line Tools (`xcode-select --install`)

### Set up environment

- install toolchains and deps

  ```bash
  mise trust && mise install
  make deps
  ```

- verify the setup by running tests

  ```bash
  make test
  ```

- run linters and formatters

  ```bash
  make lint
  ```

Now you can run the app in dev mode with hot reload using `make dev`.

### Build

Before running the build, you need to set up the dev environment.

```bash
make build
```

The version defaults to `0.0.0`. Override it for a release build:

```bash
make build VER=1.2.3
```

> `.app` bundle will be at `src-tauri/target/release/bundle/macos/`.
