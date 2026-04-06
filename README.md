# Pawkit

A desktop pet app for Windows that lives on your screen. Right-click to trigger customizable quick actions (shell commands, URLs, HTTP requests, pipelines). All configuration via YAML files — designed to be maintained by AI agents like Claude Code. Built with Tauri v2 + Vue 3.

## Features

- **Transparent always-on-top pet** — draggable pixel cat on your desktop
- **Right-click quick actions** — run shell commands, open URLs, send HTTP requests, or chain them into pipelines
- **YAML configuration** — edit `config/actions.yaml` to add/modify actions, hot-reloaded on save
- **Sound effects** — the pet meows when you interact with it
- **System tray** — show/hide/quit from the tray icon
- **Agent-friendly** — comprehensive docs in `CLAUDE.md` and `docs/` so AI agents can maintain the project

## Quick Start

### Prerequisites

- [Node.js](https://nodejs.org/) >= 18
- [pnpm](https://pnpm.io/) >= 8
- [Rust](https://rustup.rs/) >= 1.70
- Windows with [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (pre-installed on Windows 10/11)

### Development

```bash
pnpm install
pnpm tauri dev
```

### Build

```bash
pnpm tauri build
```

Output in `src-tauri/target/release/bundle/`.

## Configuration

### Actions (`config/actions.yaml`)

```yaml
actions:
  - id: build
    name: "Build Project"
    icon: "🔨"
    type: shell
    command: "pnpm build"
    workdir: "/path/to/your/project"
    group: "Dev"

  - id: open-docs
    name: "Open Docs"
    icon: "📖"
    type: url
    url: "https://example.com/docs"
```

Supported action types: `shell`, `script`, `url`, `http`, `pipeline`, `meow`

See [docs/CONFIG.md](docs/CONFIG.md) for full reference.

### Pet Appearance (`config/pet.yaml`)

```yaml
pet:
  sprite: "pixel-cat"
  scale: 2
  idle_timeout: 300
  start_position: "bottom-right"
```

## Documentation

| File | Description |
|------|-------------|
| [CLAUDE.md](CLAUDE.md) | Entry point for AI agents — project overview and maintenance guide |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture and module design |
| [docs/CONFIG.md](docs/CONFIG.md) | Configuration file format reference |
| [docs/ACTIONS.md](docs/ACTIONS.md) | Action types and how to extend |
| [docs/SPRITES.md](docs/SPRITES.md) | Sprite system and animation state machine |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Development environment setup |

## Tech Stack

- **Tauri v2** — lightweight desktop app framework (~5MB bundle)
- **Vue 3** + TypeScript — frontend rendering
- **Rust** — backend for command execution, config management, system tray
- **Canvas** — procedural pixel art animation

## License

MIT
