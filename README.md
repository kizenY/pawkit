# 🐾 Pawkit

**Unleash Claude Code from the terminal. Put your AI Agent in your pocket.**

[![GitHub stars](https://img.shields.io/github/stars/kizenY/pawkit?style=flat-square)](https://github.com/kizenY/pawkit/stargazers)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg?style=flat-square)](https://opensource.org/licenses/MIT)

**[中文文档](README_CN.md)**

---

## 😫 Love it, but hate the friction?

If you use Claude Code or similar AI CLI tools, you know the pain:

* **"Screen-watching" anxiety** — AI is writing code, but you're glued to the screen waiting for `Approve? [y/n]`.
* **Physically stuck** — Want to step out mid-session? Too bad, you need to babysit the terminal.
* **Can't reach it remotely** — You're out, and suddenly need your home machine to run a test or fix a bug. The terminal is unreachable.

**Pawkit exists to end these frustrations.**

A desktop pet cat + remote control for Claude Code. Built with Tauri v2 + Vue 3 + Rust.

---

## 🚀 Core Features

### 📱 Stop Babysitting the Terminal

Pawkit runs as Claude Code's Auth Proxy (`localhost:9527`), intercepting all tool permission requests:

- **Safe tools auto-approved** — Read, Glob, Grep and other read-only tools pass silently
- **Smart Bash analysis** — `ls`, `git status` auto-approved; `rm`, `git push`, `sudo` require approval
- **Desktop cat popup** — Unsafe tools show Allow / Allow All / Deny on the cat, without blocking the terminal
- **Allow All** — One-click to auto-approve all future requests for that tool type (current session)

### 🌍 Code From Anywhere

Right-click the cat → "Away Mode". Pawkit connects to your phone via Slack Socket Mode:

- **Slack DM chat** — Talk to Claude Code from your phone, send instructions
- **Remote approval** — Dangerous operations approved via Slack buttons (updated in-place, no spam)
- **Session inheritance** — Automatically resumes your last terminal session
- **Output forwarding** — Claude Code results pushed to Slack in real-time
- **Thread management** — New messages start new sessions, thread replies continue the current one

### 🔔 Status Notifications

AI finished a long task, or stuck waiting for approval?

- **Home mode**: Cat gets a bell icon, click to dismiss
- **Away mode**: Results pushed directly to Slack

### 🤖 Auto Review

Pawkit polls GitHub every 5 minutes:

1. Discovers PRs requesting your review, or comments @mentioning you
2. Cat popup (or Slack notification) lets you choose Handle / Skip
3. Handle → Claude Code reads the diff, analyzes, and submits a review
4. Every action (review, comment, merge) goes through the Auth Proxy for your approval

### ⚡ Custom Quick Actions

Right-click the cat to trigger actions defined in `config/actions.yaml` (hot-reloaded):

```yaml
actions:
  - id: deploy-dev
    name: "Server → Dev"
    icon: "🚀"
    type: shell
    command: "gh workflow run deploy_dev.yml --repo MyOrg/my-repo --ref main"
    group: "Deploy"

  - id: deploy-prod
    name: "Server → Prod"
    icon: "🔴"
    type: shell
    command: "gh workflow run deploy_prod.yml --repo MyOrg/my-repo --ref main -f confirm=deploy-prod"
    confirm: true
    group: "Deploy"
```

Supported action types: `shell`, `script`, `url`, `http`, `pipeline`

---

## 🔧 Getting Started

### Install

Download the latest installer from [Releases](../../releases).

### Development

```bash
git clone https://github.com/kizenY/pawkit.git
cd pawkit
pnpm install
pnpm tauri dev
```

### Build

```bash
pnpm tauri build
```

### Configure Claude Code Hook

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

### Configure Slack Remote Mode

`config/slack.yaml`:

```yaml
bot_token: "xoxb-..."
app_token: "xapp-..."
dm_user_id: "U..."
working_dir: "E:\\develop\\code"
critical_tools:
  - Bash
```

Required Slack app scopes: `chat:write`, `im:history`, `im:read`, `im:write`, `connections:write`. Add `assistant:write` for typing indicator support.

### Configure Auto Review

`config/auto_review.yaml`:

```yaml
enabled: true
interval_minutes: 5
repos:
  - MyOrg/my-repo
repo_dirs:
  MyOrg/my-repo: "C:\\projects\\my-repo"
```

---

## 🤝 Contributing

This project was born from a developer's own frustrations. If you share them, join in:

* **Feature request** — Open an Issue with your pain point
* **Found a bug** — PRs welcome
* **Roadmap**:
    - [ ] macOS / Linux support
    - [ ] Deeper integration with more CLI agents (Aider, etc.)
    - [ ] Mobile quick-action panel
    - [ ] Real-time terminal log replay

---

## ⭐ Don't forget to Star!

If Pawkit saved you from staring at a black terminal, or let you step out for a coffee in peace, please give it a **Star**!

---

## 📄 License

Distributed under the MIT License.
