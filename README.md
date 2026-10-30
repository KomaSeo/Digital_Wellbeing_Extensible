# Digital Wellbeing (Windows)

Windows desktop app that enforces a **blacklist** or **whitelist** of programs. Unauthorized processes are terminated as soon as they appear. Policies can be changed in the UI or via live **function calls** (JSON-RPC on `127.0.0.1:8765`).

## Requirements

- Rust (Cargo)
- Windows 10/11
- Run as Administrator (needed to terminate many processes)

## Build & run

```powershell
cargo run -p digital-wellbeing
```

Policy file:

`%AppData%\DigitalWellbeing\DigitalWellbeing\config\policy.json`

## Modes

| Mode | Behavior |
|------|----------|
| `blacklist` | Kill processes whose executable name is on the blacklist |
| `whitelist` | Kill everything **not** on the whitelist (system-critical names are always protected) |

## AI focus rule

Tell the app what you are working on and press **Start session** in the *AI focus* tab.
While the session runs:

1. Every 5 s the foreground window (process + title) is recorded into an activity history.
2. Every *check interval* (default 60 s) the task, that history and a downscaled
   screenshot of the active monitor are sent to Claude, which answers
   `on_task` / `off_task` / `unsure`.
3. After *N* consecutive `off_task` verdicts (default 2) the app is blocked for
   *block minutes* (default 30) — the enforcer kills it on sight. For browsers
   (`chrome.exe`, `msedge.exe`, `whale.exe`, …) only the active tab is closed (Ctrl+W),
   and a reopened tab with the same title is closed again.

`unsure` never blocks. Changing the task or stopping the session clears all AI blocks.

Settings (`%AppData%\DigitalWellbeing\DigitalWellbeing\config\ai_focus.json`):

| Setting | Meaning |
|---------|---------|
| API key | `ANTHROPIC_API_KEY` env var, or saved in settings (plain text) |
| Model | default `claude-opus-5-5`; any Claude model id works |
| Always allow | never judged or blocked |
| Private apps | judged from name + title only, never screenshotted (password managers by default) |
| Send screenshot | turn off to send titles only |

Each check is one Messages API call with one image (~1.2k input tokens at 1280 px wide),
so the interval is the main cost knob.

Code walkthrough (Korean): [`docs/ai_focus_rule.md`](docs/ai_focus_rule.md).

## Machine install (admin + standard work account)

**Easiest:** download `DigitalWellbeing-Setup-<version>.exe` from the GitHub
Releases page and run it; it asks for the work account and registers the
service. (Releases are built by `.github/workflows/release.yml` when a `v*`
tag is pushed, or when a branch push changes `installer/RELEASE_VERSION` — the
workflow then creates the `v<version>` tag itself.)

Run the blocker as a Windows service so a standard account cannot stop it, and
start it at boot:

```powershell
cargo build --release
# administrator terminal:
.\target\release\digital-wellbeing-service.exe install --manage <work-account-user>
```

The service (LocalSystem) starts a SYSTEM agent inside each managed user's
session; the agent enforces the rules and runs the AI check there. The user
declares their task in **Digital Wellbeing Focus** (starts at logon); settings
are edited with `digital-wellbeing.exe` (administrator) and stored in
`%ProgramData%\DigitalWellbeing`, which standard users cannot read or write.
Full guide (Korean): [`docs/multi_account.md`](docs/multi_account.md).

Testing changes on your own PC (much faster than a release): from an
administrator PowerShell, `.\scripts\dev-install.ps1 -Manage <work-account>`
builds and (re)installs; run it again after each change, add `-Tail` to follow
the agent log.

**Task planner** (machine install): the admin keeps a to-do list (deadline,
priority, notes) in the *Task planner* tab. When it is on, the AI picks the
user's task from the list ("Get my task" in the Focus app) and that becomes the
AI focus task; finishing, switching or ending needs a reason the AI approves.
Every request is logged for the admin. Guide (Korean):
[`docs/task_planner.md`](docs/task_planner.md).

**Access requests**: from the Focus app the user can ask for an app or site
the AI check blocks, for some minutes or permanently, stating the purpose.
If the AI approves, the block is lifted, but the app stays observed and is
judged against that purpose; off-purpose use revokes the grant and blocks
again. Blocks do not expire: an approved request or the admin lifts them (the
admin UI lists every user's blocks with an "unblock" button). The admin sets
limits and can revoke grants; the admin's blacklist is never lifted this way. Guide (Korean):
[`docs/access_grants.md`](docs/access_grants.md).

**Claude Desktop** (machine install): `digital-wellbeing-mcp.exe` is an MCP
server for Claude Desktop in the work account. Its tools show the list and the
current task, propose a to-do and ask for a change, all through the agent API
the Focus app uses, so the assistant can do nothing the user cannot. Proposed
to-dos join the list only if the agent's AI approves them against the admin's
life rules (Task planner tab); the task-picking prompt is fixed in the code.
Setup (Korean): [`docs/claude_desktop.md`](docs/claude_desktop.md).

## Function call API

JSON-RPC over TCP `127.0.0.1:8765`. One JSON object per connection.

### Methods

| Method | Params | Description |
|--------|--------|-------------|
| `ping` | `{}` | Health check |
| `get_policy` | `{}` | Current mode + lists + strategies |
| `set_mode` | `{ "mode": "blacklist" \| "whitelist" }` | Switch enforcement mode |
| `add_blacklist` | `{ "name": "steam.exe" }` | Add to blacklist |
| `remove_blacklist` | `{ "name": "steam.exe" }` | Remove from blacklist |
| `set_blacklist` | `{ "names": ["steam.exe", "discord.exe"] }` | Replace entire blacklist |
| `add_whitelist` | `{ "name": "code.exe" }` | Add to whitelist |
| `remove_whitelist` | `{ "name": "code.exe" }` | Remove (protected names rejected) |
| `set_whitelist` | `{ "names": ["code.exe", "notepad.exe"] }` | Replace entire whitelist (protected names kept) |
| `list_processes` | `{}` | Running process snapshot |
| `list_blocks` | `{ "limit": 50 }` | Recent terminations (name, pid, time) |
| `save_strategy` | `{ "name": "focus" }` | Snapshot current lists as a named strategy |
| `apply_strategy` | `{ "name": "focus" }` | Load a saved strategy |

### Example (PowerShell helper)

```powershell
# App must be running first
.\scripts\call.ps1 -Method ping
.\scripts\call.ps1 -Method set_mode -Params @{ mode = "blacklist" }
.\scripts\call.ps1 -Method add_blacklist -Params @{ name = "steam.exe" }
.\scripts\call.ps1 -Method set_blacklist -Params @{ names = @("steam.exe", "discord.exe") }
.\scripts\call.ps1 -Method set_whitelist -Params @{ names = @("code.exe", "notepad.exe") }
.\scripts\call.ps1 -Method get_policy
.\scripts\call.ps1 -Method save_strategy -Params @{ name = "evening" }
.\scripts\call.ps1 -Method apply_strategy -Params @{ name = "evening" }
```

### Example (raw)

```powershell
$json = '{"id":1,"method":"add_blacklist","params":{"name":"discord.exe"}}'
# send $json to 127.0.0.1:8765 and read one JSON response line
```

## Project layout

```
apps/desktop           egui shell: rule tabs + shared process/block views
crates/list_based_rule blacklist/whitelist BlockRule + RuleUi settings
crates/ai_focus_rule   AI-judged focus BlockRule + RuleUi (activity sampler, Claude judge)
crates/task_planner    AI picks the task from a to-do list; reviews change requests
crates/machine         machine-install paths, managed users, session ids, agent<->Focus API
apps/service           Windows service, per-session agent, installer
apps/focus             Focus app for standard users (declare task, see verdicts)
crates/enforcer        process loop + BlockRule registry
crates/rule_ui         RuleUi trait (per-rule settings panels)
crates/tool-api        JSON-RPC function-call router
scripts/call.ps1       convenience client
```

`Enforcer` has **no** dependency on `ListStore`. Callers register one or more
`BlockRule` implementations (`ListBasedRule` today; browser-extension grace
rules later). A process is terminated when **any** rule returns `Block`.
With zero rules registered, the enforcer terminates nothing (aside from never
touching OS-critical / self processes).

The desktop shell lists each registered [`RuleUi`] tab and asks that rule to
draw its own settings. New extensible rules should implement both `BlockRule`
and `RuleUi`, then register `rule()` with the enforcer and `ui()` with the app.

## Safety notes

- Critical Windows processes (`csrss.exe`, `winlogon.exe`, `explorer.exe`, …) are never killed.
- **Whitelist mode is aggressive** — only allowlisted apps (plus protected system processes) may run.
- Changes via function call are written to disk immediately and applied on the next enforcer tick (~0.8s).
