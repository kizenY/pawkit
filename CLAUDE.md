# Pawkit - Claude Code Maintenance Guide

Pawkit is a desktop pet application for Windows (with future macOS/Linux support). The pet lives on the desktop as a transparent always-on-top window. Users right-click the pet to trigger customizable quick actions (shell commands, URLs, HTTP requests, pipelines).

## Tech Stack

- **Framework**: Tauri v2 (Rust backend + Web frontend)
- **Frontend**: Vue 3 + TypeScript + Canvas (sprite animation)
- **Config**: YAML files in `config/` directory
- **Build**: pnpm + Cargo

## Project Structure

```
pawkit/
├── CLAUDE.md              # THIS FILE - Claude Code reads this first
├── docs/
│   ├── ARCHITECTURE.md    # System architecture and module design
│   ├── CONFIG.md          # Configuration file format reference
│   ├── ACTIONS.md         # Action types and how to add new ones
│   └── SPRITES.md         # Sprite assets and animation system
├── src-tauri/             # Rust backend
│   ├── src/
│   │   ├── main.rs        # Entry point: CLI subcommands or GUI launch
│   │   ├── cli.rs         # CLI mode (list, run <action-id>)
│   │   ├── executor.rs    # Runs shell/http/pipeline actions
│   │   ├── config.rs      # YAML config read/write + file watcher
│   │   ├── tray.rs        # System tray icon and menu
│   │   └── notifier.rs    # Windows Toast notifications
│   ├── tauri.conf.json    # Tauri window config (transparent, no decorations)
│   └── Cargo.toml
├── src/                   # Vue 3 frontend
│   ├── components/
│   │   ├── Pet.vue        # Pet sprite renderer + animation state machine
│   │   └── ContextMenu.vue # Right-click action menu
│   ├── composables/
│   │   └── useActions.ts  # Reactive action list from backend
│   ├── assets/sprites/    # Sprite sheet PNGs
│   ├── App.vue
│   └── main.ts
├── config/
│   ├── actions.yaml       # User-defined actions (THE file Claude Code edits)
│   └── pet.yaml           # Pet appearance settings
└── package.json
```

## Key Maintenance Tasks

### Adding/Modifying Actions
Edit `config/actions.yaml`. See `docs/CONFIG.md` for full format reference.

### Changing Pet Appearance
Edit `config/pet.yaml`. See `docs/SPRITES.md` for available sprites.

### Adding New Action Types
1. Add type handler in `src-tauri/src/executor.rs`
2. Update TypeScript types in `src/composables/useActions.ts`
3. Update `docs/ACTIONS.md`

### Building
```bash
pnpm install
pnpm tauri build
```

### Development
```bash
pnpm install
pnpm tauri dev
```

### CLI Mode
The binary supports CLI subcommands without launching the GUI:
```bash
pawkit list                    # List all available actions
pawkit list -g "Deploy"        # Filter by group
pawkit run <action-id>         # Execute an action
pawkit run <action-id> -y      # Skip confirmation prompt
pawkit                         # No subcommand → launch GUI as usual
```

## Important Conventions
- All config files use YAML format
- Action IDs must be unique, lowercase, kebab-case
- Config changes are hot-reloaded (no restart needed)
- Platform-specific code is isolated behind traits/interfaces
- Never hardcode Windows paths; use `dirs` crate for system paths

## Session Behavior Constraints (Pawkit)
- NEVER present multi-select or interactive prompts. Always choose the best option automatically.
- Do not use interactive commands: `git add -i`, `npm init` (without `-y`), `git rebase -i`, etc.
- If you need user input, use the AskUserQuestion tool (which routes through Pawkit's approval flow).
- If a decision genuinely requires the user, end this conversation turn with a clear question, and the user will respond in the next turn.
