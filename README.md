# Meerkat

A macOS menubar app for tracking GitLab merge requests.

Meerkat lives in your system tray, periodically polls GitLab for MRs where you are a reviewer,
assignee, or mentioned, and sends notifications when something changes.

- [Features](#features)
- [Installation](#installation)
- [Development](#development)

## Features

- System tray icon with unread MR badge
- Native macOS notifications with sound and click-to-open for new review
  requests, review re-requests, MR updates, and failed pipelines
- Read/unread tracking that ignores your own actions; marking an MR read by
  hand can be flipped back to unread by a new re-request or a fresh comment
  from someone else
- Periodic background polling with configurable interval
- MR filtering by project and role (reviewer / assignee / mentioned)
- Custom reminders for individual merge requests
- Detail panel with activity timeline
- Hides to tray on window close
- Light and dark theme (follows system)

## Installation

Download the latest release from [releases](https://github.com/aleksey925/meerkat/releases)
and install it manually.

> **Note:** Release builds are not signed with an Apple Developer certificate, so macOS Gatekeeper
> will show a warning that the app is damaged or can't be opened. To fix this, run:
>
> ```bash
> xattr -cr /Applications/Meerkat.app
> ```
>
> Alternatively, you can [build from source](#build) on your machine to avoid this issue.

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

Now you can run the app in dev mode with hot reload using `make dev`.

### Build

Before running the build, you need to set up the dev environment.

```bash
make build
```

> `.app` bundle will be at `src-tauri/target/release/bundle/macos/`.
