# Pawkit

A desktop pet that doubles as a Claude Code companion. The pixel cat sits on your desktop, proxies Claude Code's permission requests, notifies you when tasks complete, and lets you control Claude Code remotely via Slack when you're away from your desk.

Built with Tauri v2 + Vue 3 + Rust. Windows only (for now).

## Features

### Desktop Pet
- Transparent, always-on-top, draggable pixel cat
- Animated states: idle, busy, success, fail, sleep, away
- Right-click context menu for quick actions
- System tray integration (show/hide/quit)
- Sound effects (meow on interaction, bell on task completion)

### Claude Code Auth Proxy
Pawkit runs an HTTP hook server on `localhost:9527` that intercepts Claude Code's tool permission requests:

- **Safe tools auto-allowed** — Read, Glob, Grep, Agent, WebSearch, etc. pass through silently
- **Smart Bash analysis** — read-only commands (`ls`, `git status`, `find`) auto-allowed; dangerous commands (`rm`, `git push`, `sudo`) require approval
- **Visual permission UI** — non-safe tools show an Allow / Allow All / Deny prompt on the cat instead of blocking the terminal
- **Allow All** — auto-allows that tool type for the rest of the session

### Bell Notifications
When Claude Code completes a task, the cat gets a bell. Click the cat to dismiss it.

### Away Mode (Slack Remote Control)
When you leave your desk, right-click the cat and select "外出模式". Pawkit connects to Slack via Socket Mode and lets you:

- **Chat with Claude Code** from your phone via Slack DM
- **Approve/deny** critical tool requests with interactive Slack buttons (updated in-place, no spam)
- **Resume your terminal session** — Pawkit remembers the last Claude Code session and continues it remotely
- **Thread-based conversations** — new top-level messages start new sessions, thread replies continue the current one
- **Built-in commands**: `!ping`, `!cd`, `!stop`, `!auto on/off`
- **Typing indicator** via Slack's Assistants API while Claude is thinking

Right-click the cat and select "回家了" to return to local mode.

### Quick Actions
Right-click the cat to trigger configurable actions defined in YAML:

| Type | Description |
|------|-------------|
| `shell` | Run shell commands |
| `script` | Execute script files (.ps1, .py, .sh) |
| `url` | Open URLs in default browser |
| `http` | Send HTTP requests (GET/POST/PUT/DELETE) |
| `pipeline` | Chain multiple steps sequentially |

Actions support grouping, environment variable substitution (`${VAR}`), confirmation dialogs, and hot-reload on config save.

### Hot-Reload Configuration
All YAML config files are watched for changes and reloaded automatically — no restart needed.

## Quick Start

### Prerequisites

- [Node.js](https://nodejs.org/) >= 18
- [pnpm](https://pnpm.io/) >= 8
- [Rust](https://rustup.rs/) >= 1.70
- Windows 10/11 with [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)

### Install

Download the latest installer from [Releases](../../releases).

### Development

```bash
pnpm install
pnpm tauri dev
```

### Build

```bash
pnpm tauri build
```

## Configuration

All config files live in `config/` and are hot-reloaded on save.

### Actions (`config/actions.yaml`)

```yaml
actions:
  - id: build
    name: "Build Project"
    icon: "🔨"
    type: shell
    command: "pnpm build"
    workdir: "/path/to/project"
    group: "Dev"
    confirm: true

  - id: open-docs
    name: "Open Docs"
    icon: "📖"
    type: url
    url: "https://example.com/docs"
```

See [docs/CONFIG.md](docs/CONFIG.md) for full reference.

### Pet Appearance (`config/pet.yaml`)

```yaml
pet:
  sprite: "pixel-cat"
  scale: 2
  idle_timeout: 300
  start_position: "bottom-right"
  opacity: 1.0
```

### Claude Code Hook Setup

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:9527/hook/pre-tool-use",
            "timeout": 120
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:9527/hook/notification",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

### Slack Remote Mode (`config/slack.yaml`)

```yaml
bot_token: "xoxb-..."
app_token: "xapp-..."
dm_user_id: "U..."
working_dir: "E:\\develop\\code"
critical_tools:
  - Bash
```

Required Slack app scopes: `chat:write`, `im:history`, `im:read`, `im:write`, `connections:write`. Add `assistant:write` for typing indicator support.

## Documentation

| File | Description |
|------|-------------|
| [CLAUDE.md](CLAUDE.md) | AI agent maintenance guide |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture and module design |
| [docs/CONFIG.md](docs/CONFIG.md) | Configuration file format reference |
| [docs/ACTIONS.md](docs/ACTIONS.md) | Action types and extension guide |
| [docs/SPRITES.md](docs/SPRITES.md) | Sprite system and animation state machine |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Development environment setup |

## Tech Stack

- **Tauri v2** — desktop framework (~5MB bundle)
- **Vue 3** + TypeScript — frontend
- **Rust** — backend (action execution, hook server, Slack bridge, window management)
- **Canvas** — procedural pixel art animation
- **Axum** — HTTP hook server
- **Tokio-Tungstenite** — Slack Socket Mode WebSocket

## License

MIT
