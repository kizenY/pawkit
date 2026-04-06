# Action Types

This document describes each action type supported by Pawkit and how to extend with new types.

## Built-in Types

### shell

Executes a shell command via the system shell (`cmd /C` on Windows, `sh -c` on Unix).

```yaml
- id: build
  name: "构建项目"
  type: shell
  command: "pnpm build"
  workdir: "/path/to/your/project"
  env:
    NODE_ENV: production
```

**Behavior:**
- Spawns a child process
- Captures stdout and stderr
- Reports exit code (0 = success, non-zero = failure)
- Killed if Pawkit exits

### script

Executes a script file. Convenience wrapper over `shell` for longer scripts.

```yaml
- id: backup
  name: "备份数据库"
  type: script
  path: "E:/scripts/backup.ps1"
  args: ["--target", "production"]
```

**Behavior:**
- Infers interpreter from extension (.ps1 → powershell, .sh → bash, .py → python)
- Same process management as `shell`

### url

Opens a URL in the system default browser.

```yaml
- id: open-jira
  name: "打开 Jira"
  type: url
  url: "https://jira.example.com/board/123"
```

**Behavior:**
- Uses `open` crate (cross-platform)
- Always succeeds unless URL is malformed

### http

Sends an HTTP request. Useful for webhooks, API triggers, CI/CD.

```yaml
- id: trigger-build
  name: "触发 CI 构建"
  type: http
  method: POST
  url: "https://api.github.com/repos/owner/repo/dispatches"
  headers:
    Authorization: "Bearer ${GITHUB_TOKEN}"
    Accept: application/vnd.github.v3+json
  body: '{"event_type": "deploy"}'
  timeout: 15
```

**Behavior:**
- Uses `reqwest` for HTTP requests
- Supports `${ENV_VAR}` syntax in headers and body for secrets
- Success: 2xx status code. Failure: anything else or timeout.
- Response body is logged

### pipeline

Executes multiple steps sequentially. Each step is a `shell`, `http`, or `url` action.

```yaml
- id: full-deploy
  name: "完整部署"
  type: pipeline
  steps:
    - type: shell
      command: "pnpm test"
      workdir: "/path/to/your/project"
    - type: shell
      command: "pnpm build"
      workdir: "/path/to/your/project"
    - type: http
      method: POST
      url: "https://deploy.example.com/trigger"
  on_failure: stop  # stop (default) | continue
```

**Behavior:**
- Steps run in order
- `on_failure: stop` — aborts pipeline on first failure
- `on_failure: continue` — runs all steps, reports failures at the end
- Each step's output is logged separately

## CLI Usage

Actions can be executed directly from the terminal without launching the GUI:

```bash
# List all actions
pawkit list

# List actions in a specific group
pawkit list -g "Deploy"

# Run an action
pawkit run deploy-dev

# Run with confirmation skipped
pawkit run deploy-prod -y
```

The CLI uses the same `config.rs` loader and `executor.rs` engine as the GUI, so behavior is identical. The exit code matches the action's exit code, making it composable in shell scripts:

```bash
pawkit run build && pawkit run deploy-staging
```

## Environment Variable Substitution

All string fields support `${ENV_VAR}` syntax:

```yaml
- id: deploy
  type: http
  url: "https://api.example.com/deploy"
  headers:
    Authorization: "Bearer ${API_TOKEN}"
```

Variables are resolved from the system environment at execution time. If a variable is not found, the literal `${VAR_NAME}` is kept and a warning is logged.

## Adding a New Action Type

To add a new action type (e.g., `docker`):

1. **Define the config fields** — Add the new variant to the `ActionType` enum in `src-tauri/src/config.rs`:

```rust
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ActionType {
    Shell { command: String, workdir: Option<String>, env: Option<HashMap<String, String>> },
    Url { url: String },
    Http { method: String, url: String, headers: Option<HashMap<String, String>>, body: Option<String>, timeout: Option<u64> },
    Pipeline { steps: Vec<ActionStep>, on_failure: Option<String> },
    // Add here:
    Docker { image: String, command: Option<String>, volumes: Option<Vec<String>> },
}
```

2. **Implement execution** — Add a match arm in `src-tauri/src/executor.rs`:

```rust
ActionType::Docker { image, command, volumes } => {
    // Build and run docker command
}
```

3. **Update TypeScript types** — Add the type to `src/composables/useActions.ts`

4. **Document** — Add a section to this file describing the new type

5. **Test** — Add an example action to `config/actions.yaml` and verify it works
