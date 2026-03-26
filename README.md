Meerkat
=======

A macOS menubar app for tracking GitLab merge requests.

Meerkat lives in your system tray, periodically polls GitLab for MRs where you are a reviewer, 
assignee, or mentioned, and sends notifications when something changes.

> This application was generated using Claude Code with no manual code review. Only manual testing was performed.

## Features

- System tray icon with unread MR badge
- Native macOS notifications with sound and click-to-open
- Periodic background polling with configurable interval
- MR filtering by project and role (reviewer / assignee / mentioned)
- Custom reminders for individual merge requests
- Detail panel with activity timeline
- Hides to tray on window close
- Light and dark theme (follows system)


## Development

**Prerequisites**

  - macOS
  - [mise](https://mise.jdx.dev) for managing Node.js and Rust toolchains
  - Xcode Command Line Tools (`xcode-select --install`)

### Setup dev environment

- install toolchains and deps

    ```bash
    mise trust && mise install
    make deps
    ```

- run app in dev mode with hot reload `make dev`

### Build

Before running the build, you need to set up the dev environment.

```bash
make build
```

> `.app` bundle will be at `src-tauri/target/release/bundle/macos/`.

> If you transfer a built app to another PC, you may receive a message 
> saying the app is broken and should be removed. This happens because 
> the app is not signed. 
> To fix it, run the following command: `xattr -cr /Applications/Meerkat.app`.
