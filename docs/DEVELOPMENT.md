# Development Guide

## Prerequisites

- [Node.js](https://nodejs.org/) >= 18
- [pnpm](https://pnpm.io/) >= 8
- [Rust](https://rustup.rs/) >= 1.70
- Tauri v2 CLI: `cargo install tauri-cli --version "^2"`
- Windows: Microsoft Visual Studio C++ Build Tools

## Setup

```bash
# Clone and install dependencies
cd pawkit
pnpm install

# Start development server (hot-reload for both frontend and backend)
pnpm tauri dev
```

## Build for Production

```bash
pnpm tauri build
```

Output: `src-tauri/target/release/bundle/` contains the installer (.msi) and portable (.exe).

## Project Init Checklist (for initial setup)

Run these commands to scaffold the project from scratch:

```bash
# 1. Create Tauri v2 + Vue 3 project
pnpm create tauri-app pawkit --template vue-ts --manager pnpm

# 2. Install additional Rust dependencies
cd src-tauri
cargo add serde serde_yaml --features serde/derive
cargo add notify --features macos_fsevent    # File watcher
cargo add reqwest --features json,blocking   # HTTP client
cargo add open                                # Open URLs
cargo add tauri-plugin-shell                  # Shell commands
cargo add chrono                              # Timestamps for logs
cargo add dirs                                # Cross-platform paths

# 3. Install frontend dependencies
cd ..
pnpm add -D @tauri-apps/api
```

## Tauri Window Config

Key settings in `src-tauri/tauri.conf.json` for the desktop pet effect:

```json
{
  "app": {
    "windows": [
      {
        "title": "Pawkit",
        "transparent": true,
        "decorations": false,
        "alwaysOnTop": true,
        "resizable": false,
        "width": 128,
        "height": 128,
        "skipTaskbar": true
      }
    ]
  }
}
```

## Testing

```bash
# Run Rust tests
cd src-tauri && cargo test

# Run frontend type check
pnpm vue-tsc --noEmit
```

## File Watcher Behavior

The config module watches `config/actions.yaml` and `config/pet.yaml`:
- On file change → re-parse YAML → emit `config_changed` event to frontend
- Frontend updates the context menu and pet appearance reactively
- Invalid YAML is logged as a warning; last valid config is kept
