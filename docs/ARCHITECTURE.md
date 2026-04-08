# Architecture

## Overview

Pawkit is a Tauri v2 desktop application with a transparent, borderless, always-on-top window that renders an animated pixel pet. Users interact via right-click context menu to trigger configurable actions.

```
┌─────────────────────────────────────────┐
│         Transparent Window (Vue 3)       │
│   ┌──────────┐   ┌──────────────────┐   │
│   │ Pet.vue  │   │ ContextMenu.vue  │   │
│   │ (Canvas) │   │ (right-click)    │   │
│   └────┬─────┘   └───────┬──────────┘   │
│        │ Tauri IPC        │              │
└────────┼──────────────────┼──────────────┘
         │                  │
┌────────▼──────────────────▼──────────────┐
│            Rust Backend                   │
│                                           │
│  ┌───────────┐  ┌──────────┐  ┌────────┐ │
│  │ config.rs │  │executor.rs│  │tray.rs │ │
│  │ YAML r/w  │  │ run cmds  │  │systray │ │
│  │ file watch│  │ shell/http│  │        │ │
│  └───────────┘  └──────────┘  └────────┘ │
│  ┌────────────┐                           │
│  │notifier.rs │                           │
│  │ Win Toast  │                           │
│  └────────────┘                           │
└───────────────────────────────────────────┘
```

## Module Responsibilities

### Frontend (src/)

| Module | Responsibility |
|--------|---------------|
| `Pet.vue` | Renders sprite sheet animation on a `<canvas>`. Manages animation state machine (idle, busy, success, fail, sleep). Handles drag-to-move. |
| `ContextMenu.vue` | Displays grouped action list on right-click. Sends selected action ID to backend via Tauri `invoke()`. |
| `useActions.ts` | Listens to backend config change events. Provides reactive `actions` list to components. |

### Backend (src-tauri/src/)

| Module | Responsibility |
|--------|---------------|
| `config.rs` | Reads/writes `config/actions.yaml` and `config/pet.yaml`. Watches files for changes and emits events to frontend. Uses `serde_yaml` + `notify` crate. |
| `executor.rs` | Executes actions by type. `shell`: spawns child process. `url`: opens default browser. `http`: sends HTTP request via `reqwest`. `pipeline`: runs steps sequentially, stops on failure. |
| `tray.rs` | Creates system tray icon with basic menu (Show/Hide, Quit). |
| `notifier.rs` | Sends Windows Toast notifications on action completion. Abstracts platform differences behind a trait for future macOS/Linux support. |
| `main.rs` | Entry point. Parses CLI args via `clap`: if a subcommand is given (`list`, `run`), runs in CLI mode; otherwise launches Tauri GUI. |
| `cli.rs` | CLI mode. `list` prints actions (with optional group filter). `run <id>` executes an action, respecting `confirm` (interactive prompt, skippable with `-y`). Reuses `config.rs` and `executor.rs`. |

## IPC Commands (Tauri invoke)

| Command | Direction | Description |
|---------|-----------|-------------|
| `get_actions` | Frontend → Backend | Returns current action list |
| `run_action` | Frontend → Backend | Executes action by ID |
| `get_pet_config` | Frontend → Backend | Returns pet appearance config |
| `config_changed` | Backend → Frontend (event) | Emitted when YAML files change |
| `action_started` | Backend → Frontend (event) | Action execution started |
| `action_finished` | Backend → Frontend (event) | Action completed (success/fail + output) |

## Cross-Platform Strategy

Platform-specific code is isolated behind traits:

```rust
trait Notifier {
    fn notify(&self, title: &str, body: &str, success: bool);
}

// Implementations per platform:
// - WindowsNotifier (win-toast-notify)
// - MacNotifier (mac-notification-sys) — future
// - LinuxNotifier (notify-rust) — future
```

File paths use the `dirs` crate (`dirs::config_dir()`) to resolve `~/.config/pawkit/` (Linux/Mac) or `%APPDATA%/pawkit/` (Windows) for production installs. During development, config is read from the project `config/` directory.

## Data Flow: Action Execution

### GUI Mode
```
1. User right-clicks pet → ContextMenu shows
2. User clicks "发版" → invoke("run_action", { id: "release" })
3. executor.rs spawns `npm run release` in configured workdir
4. Backend emits "action_started" → Pet.vue switches to "busy" animation
5. Process completes → Backend emits "action_finished" with result
6. Pet.vue switches to "success"/"fail" animation
7. notifier.rs shows Windows Toast notification
8. stdout/stderr logged to ~/.pawkit/logs/
```

### CLI Mode
```
1. User runs: pawkit run release
2. cli.rs loads config/actions.yaml, finds action by ID
3. If confirm: true → interactive prompt (skipped with -y flag)
4. executor.rs executes the action (same code path as GUI)
5. stdout/stderr printed to terminal
6. Process exits with the action's exit code
```
