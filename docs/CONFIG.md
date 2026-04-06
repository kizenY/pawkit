# Configuration Reference

All configuration lives in the `config/` directory as YAML files. Changes are hot-reloaded — no restart needed.

Claude Code (or any agent) maintains these files by directly editing them. No UI or API is needed.

---

## config/actions.yaml

Defines all quick actions available in the pet's right-click menu.

### Full Schema

```yaml
actions:
  - id: string            # REQUIRED. Unique ID, lowercase kebab-case (e.g. "deploy-staging")
    name: string           # REQUIRED. Display name in menu (e.g. "部署测试环境")
    icon: string           # Optional. Emoji icon shown in menu (e.g. "🚀")
    type: string           # REQUIRED. One of: shell, script, url, http, pipeline
    group: string          # Optional. Menu group name (e.g. "部署", "工具")
    confirm: boolean       # Optional. If true, show confirmation dialog before executing. Default: false
    enabled: boolean       # Optional. If false, action is hidden from menu. Default: true

    # --- Type-specific fields ---

    # type: shell
    command: string        # Shell command to execute
    workdir: string        # Optional. Working directory (absolute path)
    env:                   # Optional. Extra environment variables
      KEY: value

    # type: script
    path: string           # Path to script file (absolute or relative to config dir)
    args: [string]         # Optional. Script arguments
    workdir: string        # Optional. Working directory

    # type: url
    url: string            # URL to open in default browser

    # type: http
    method: string         # HTTP method: GET, POST, PUT, DELETE
    url: string            # Request URL
    headers:               # Optional. HTTP headers
      Content-Type: application/json
    body: string           # Optional. Request body (string)
    timeout: number        # Optional. Timeout in seconds. Default: 30

    # type: pipeline
    steps:                 # Array of actions to execute sequentially
      - type: shell
        command: string
        workdir: string
      - type: http
        method: POST
        url: string
        body: string
    on_failure: string     # Optional. "stop" (default) or "continue"
```

### Example

```yaml
actions:
  - id: release
    name: "发版"
    icon: "🚀"
    type: shell
    command: "npm run release"
    workdir: "/path/to/your/project"
    confirm: true
    group: "部署"

  - id: open-grafana
    name: "监控面板"
    icon: "📊"
    type: url
    url: "https://grafana.example.com"
    group: "工具"

  - id: deploy-staging
    name: "部署测试环境"
    icon: "🧪"
    type: pipeline
    confirm: true
    group: "部署"
    steps:
      - type: shell
        command: "pnpm build"
        workdir: "/path/to/your/project"
      - type: shell
        command: "pnpm deploy:staging"
        workdir: "/path/to/your/project"
      - type: http
        method: POST
        url: "https://hooks.slack.com/services/xxx"
        headers:
          Content-Type: application/json
        body: '{"text": "Staging deployed successfully"}'
    on_failure: stop

  - id: kill-port-3000
    name: "释放 3000 端口"
    icon: "🔪"
    type: shell
    command: "npx kill-port 3000"
    group: "工具"

  - id: git-sync
    name: "同步代码"
    icon: "🔄"
    type: pipeline
    group: "Git"
    steps:
      - type: shell
        command: "git fetch --all && git pull --rebase"
        workdir: "/path/to/your/project"
```

### Rules
- `id` must be unique across all actions
- `id` must be lowercase kebab-case: `[a-z0-9]+(-[a-z0-9]+)*`
- `group` is used to organize the right-click menu into submenus (GUI) and filter output (CLI `pawkit list -g`)
- Actions without a `group` appear at the top level of the menu
- Actions are displayed in the order they appear in the file
- `id` is used as the CLI identifier: `pawkit run <id>`

---

## config/pet.yaml

Controls the pet's appearance and behavior.

```yaml
pet:
  sprite: "pixel-cat"          # Sprite set name (folder name under assets/sprites/)
  scale: 2                     # Render scale multiplier (1 = original size)
  idle_timeout: 300            # Seconds of inactivity before pet falls asleep
  start_position: "bottom-right"  # Initial position: bottom-right, bottom-left, center, or [x, y]
  opacity: 1.0                 # Window opacity (0.0 - 1.0)
  click_through: false         # If true, clicks pass through the pet to windows below (except right-click)
```

### Available Sprites

See `docs/SPRITES.md` for the full list of available sprite sets and how to add new ones.

---

## Logs

Execution logs are stored in:
- Development: `./logs/`
- Production: `~/.pawkit/logs/`

Log file format: `{action-id}_{timestamp}.log`

Each log file contains:
```
[INFO] Action started: release (2026-04-06 14:30:00)
[INFO] Command: npm run release
[INFO] Workdir: /path/to/your/project
[STDOUT] ...
[STDERR] ...
[INFO] Exit code: 0
[INFO] Duration: 12.3s
```
