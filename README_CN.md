# 🐾 Pawkit

**让 Claude Code 脱离终端束缚，把你的 AI Agent 装进兜里。**

[![GitHub stars](https://img.shields.io/github/stars/kizenY/pawkit?style=flat-square)](https://github.com/kizenY/pawkit/stargazers)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg?style=flat-square)](https://opensource.org/licenses/MIT)

**[English](README.md)**

---

## 😫 用的很爽，但也很烦？

如果你在使用 Claude Code 或类似的 AI CLI 编程工具，你一定经历过这些痛苦：

* **"盯盘"焦虑**：明明 AI 在写代码，你却得一直死守在屏幕前，等它弹出一个 `Approve? [y/n]`。
* **物理束缚**：代码写到一半想出门？对不起，你得守在电脑前。
* **远程无力**：人在外面，突然想让家里的电脑跑个测试、改个 Bug，终端却不可触达。

**Pawkit 就是为了终结这些困扰而生的。**

一只桌面小猫 + Claude Code 的远程控制手柄。基于 Tauri v2 + Vue 3 + Rust 构建。

---

## 🚀 核心功能

### 📱 不盯盘的 AI 编程

Pawkit 作为 Claude Code 的 Auth Proxy（`localhost:9527`），接管所有工具权限请求：

- **安全工具自动放行** — Read、Glob、Grep 等只读操作静默通过
- **智能 Bash 分析** — `ls`、`git status` 自动放行；`rm`、`git push`、`sudo` 需要审批
- **桌面小猫弹窗** — 非安全工具在小猫上弹出 Allow / Allow All / Deny，不再阻塞终端
- **Allow All** — 一键放行该工具类型的所有后续请求（本次会话内）

### 🌍 出门也能 Coding

右键小猫 →「外出模式」，Pawkit 通过 Slack Socket Mode 连接你的手机：

- **Slack DM 对话** — 在手机上直接和 Claude Code 聊天，发指令
- **远程审批** — 危险操作通过 Slack 按钮审批（原地更新，不刷屏）
- **会话继承** — 自动恢复你离开前的终端会话
- **输出转发** — Claude Code 的任务结果实时推送到 Slack
- **线程管理** — 新消息开新会话，thread 回复延续当前会话

### 🔔 状态推送

AI 完成了长任务，或者卡在了某个权限申请？

- **回家模式**：小猫挂上铃铛，点击消除
- **外出模式**：结果直接推送到 Slack

### 🤖 Auto Review

Pawkit 每 5 分钟自动巡查 GitHub：

1. 发现需要你 review 的 PR 或 @mention 你的评论
2. 小猫弹窗（或 Slack 通知）让你选择 Handle / Skip
3. 点 Handle → Claude Code 自动读 diff、分析、提交 review
4. 每个动作（review、comment、merge）都经过 Auth Proxy 等你审批

### ⚡ 自定义快捷操作

右键小猫触发你定义的快捷操作，`config/actions.yaml` 热加载：

```yaml
actions:
  # 一键部署
  - id: deploy-dev
    name: "Server → Dev"
    icon: "🚀"
    type: shell
    command: "gh workflow run deploy_dev.yml --repo MyOrg/my-repo --ref main"
    group: "Deploy"

  # 危险操作带确认框
  - id: deploy-prod
    name: "Server → Prod"
    icon: "🔴"
    type: shell
    command: "gh workflow run deploy_prod.yml --repo MyOrg/my-repo --ref main -f confirm=deploy-prod"
    confirm: true
    group: "Deploy"
```

支持的 action 类型：`shell`、`script`、`url`、`http`、`pipeline`

---

## 🔧 快速开始

### 安装

从 [Releases](../../releases) 下载最新安装包。

### 开发

```bash
git clone https://github.com/kizenY/pawkit.git
cd pawkit
pnpm install
pnpm tauri dev
```

### 构建

```bash
pnpm tauri build
```

### 配置 Claude Code Hook

将以下内容添加到 `~/.claude/settings.json`：

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

### 配置 Slack 远程模式

`config/slack.yaml`：

```yaml
bot_token: "xoxb-..."
app_token: "xapp-..."
dm_user_id: "U..."
working_dir: "E:\\develop\\code"
critical_tools:
  - Bash
```

所需 Slack App 权限：`chat:write`、`im:history`、`im:read`、`im:write`、`connections:write`。添加 `assistant:write` 以支持输入状态指示。

### 配置 Auto Review

`config/auto_review.yaml`：

```yaml
enabled: true
interval_minutes: 5
repos:
  - MyOrg/my-repo
repo_dirs:
  MyOrg/my-repo: "C:\\projects\\my-repo"
```

---

## 🤝 参与贡献

这是一个为了解决"开发者自己的烦恼"而诞生的项目。欢迎加入：

* **提个需求**：开个 Issue 告诉我你最想解决的痛点
* **修个 Bug**：欢迎直接提 PR
* **Roadmap**：
    - [ ] macOS / Linux 支持
    - [ ] 深度适配更多 CLI Agent (Aider 等)
    - [ ] 手机端快捷操作面板
    - [ ] 实时终端日志回溯

---

## ⭐ 别忘了给个 Star！

如果 Pawkit 帮你省下了盯着黑框框的时间，或者让你能安心出门喝杯咖啡，请点个 **Star** 支持一下！

---

## 📄 License

Distributed under the MIT License.
