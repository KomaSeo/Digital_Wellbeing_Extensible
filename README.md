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

A list entry is a program name, or a case-insensitive regular expression that must
match the whole name when it contains any of `* + ? ( ) [ ] { } | ^ $ \` (e.g.
`.*game.*\.exe`; a lone dot does not count, so `steam.exe` stays a plain name; an
invalid expression is matched as plain text). Windows essentials and this program's
own executables are never killed, whatever a pattern matches.

## AI focus rule

The AI checks judge every window against the current task ("daily wellbeing").
In a machine install the task planner's AI always picks that task from the to-do
list and turns the checks on; the user never types it (the Focus app's UI is in
Korean; the change request 「AI 검사 멈추기」 pauses the checks if the AI approves
the reason). In the standalone app the user types the task and presses **Start
session** in the *AI focus* tab. While the AI checks are on:

1. Every 3 s the open windows are sampled; the foreground one goes into an activity history.
2. Any app or page without a verdict for the current task is judged at once: the task,
   that history, a downscaled screenshot of the foreground monitor and the titles of the
   other open windows go to Claude, which answers `on_task` / `off_task` / `unsure` for
   each. A verdict comes due again after *recheck minutes* (on_task, default 15) or the
   *check interval* (unsure, or an app still collecting strikes; default 60 s).
3. A browser tab judged `off_task` is closed (Ctrl+W) and its site blocked at once
   (YouTube: the channel). An app needs *N* consecutive `off_task` verdicts (default 2);
   then the enforcer kills it on sight.

`unsure` never blocks. Blocks do not expire and survive task changes; an approved
access request (or `unblock`) lifts them.

**Always-blocked websites** (`always_blocked_sites` in `ai_focus.json`): a browser
tab whose title contains an entry (plain text, or a regular expression searched in
the title; case-insensitive; the trailing browser name such as " - Google Chrome"
is ignored) is closed at once without asking the AI, whether or not the AI checks
are on, and background browser windows showing one are minimized. Access requests
do not lift it.

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

## Machine install (standard work account; administrator only to install)

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
works in **Digital Wellbeing Focus** (starts at logon; Korean UI; tabs 할 일
(to-do), 차단 (blocking), 설정 (settings), 통계 (statistics: AI usage, today,
finished to-dos, requests and answers, recent AI verdicts) and 도움말 (help,
including a Claude Desktop guide); tray menu 열기 / 설정 / 끝내기). Settings are stored in `%ProgramData%\DigitalWellbeing`, which
standard users cannot read or write directly. The settings the app needs (AI connection:
API key, model, daily budget; the rules for the AI: life rules, the planner's
instructions; blocking: always-blocked programs and websites, blocks and unblock,
access grants and revoke, access limits; the AI checks) are edited in the Focus
app's **설정** (Settings) tab, from a managed (standard) account: the tab reads them through
that session's agent, which writes the real files as SYSTEM. The program does
not tell administrator and standard accounts apart; only the **settings
protection** decides. During **setup** (right after installing) an edit takes
effect at once, and anyone on the PC can make it. The user ends setup with
「설정 보호하기」 (protect the settings) in the Settings tab; this is one way, and nothing in the
program turns it off. From then on a saved file (from the Settings tab or
Claude Desktop) is split field by field (`machine::setting_edits`): edits that
make the rules **stricter** (blacklist an app, add an always-blocked website, fewer
strikes, shorter recheck times, screenshots on, fewer always-allowed or private
apps, revoke a grant, a larger or no daily budget, no permanent access) take effect at once;
everything else, including what cannot be judged (API key, model, life rules,
instructions, browser list), becomes a **scheduled update**: two days later the
Focus app asks 「이 업데이트가 정말 맞나요?」 ("is this update really right?"), each "yes" brings the next
question two days after the answer, and the third "yes" in a row applies the
edits to the file as it is then (so at least six days); one "no" rejects it,
and an unanswered question waits. Edits in the Settings tab are autosaved as
drafts (field-level edits, never the API key) a second after each change to
`settings_drafts.json` in the user's local app data folder and restored when the
app starts; nothing takes effect until the user presses 「적용」 (apply) under that
group of settings, which sends only that group's edits on top of the current file.
Each group's bar marks every pending edit as taking effect at once or scheduled,
and offers 저장 (save the draft now), 고친 것 버리기 (discard) and, where the
program has a default, 기본값으로 되돌리기 (back to default). The tab shows what
each scheduled update changes and can withdraw it. Windows cannot stop an administrator account from
stopping the service or deleting `settings_protection.json`, so work in a
standard account and use the administrator
account only to install and to change which accounts are managed
(`install --manage` / `machine.json`, which the Settings tab cannot edit).

A machine install ships three executables (`digital-wellbeing-service.exe`,
`digital-wellbeing-focus.exe`, `digital-wellbeing-mcp.exe`); it no longer ships
the old admin settings window `digital-wellbeing.exe`, and `install` deletes a
copy an older version left. Run on a PC with the service installed, a
`digital-wellbeing.exe` built from source only shows a notice pointing to the
Focus app's Settings tab (`--standalone` still forces single-account mode).
Full guide (Korean): [`docs/multi_account.md`](docs/multi_account.md).

Testing changes on your own PC (much faster than a release): from an
administrator PowerShell, `.\scripts\dev-install.ps1 -Manage <work-account>`
builds and (re)installs; run it again after each change, add `-Tail` to follow
the agent log.

**Task planner** (machine install): the user keeps a to-do list (deadline,
priority, estimate, notes) in the Focus app's 할 일 (to-do) tab, and the life rules
in the Settings tab. Every addition, edit or removal of a to-do goes to the
AI, which reviews it against the life rules and fills in the fields of a new
to-do the user left empty; an approved change is in force at once, even when
the settings are protected. A rejected one can still be done by the user (not
by Claude Desktop) after three confirmations two hours apart. The planner
is always on (there is no switch and no mode where the user types the task): while
any to-do is open, the AI picks the user's task from the list (the agent does so
by itself every 15 s, or 「할 일 받기」) and that becomes the task the AI checks
judge against. Finishing the task, getting something else or pausing the AI checks
needs a reason the AI approves; the user never chooses what comes next. "Something
else" (「다른 일 받기」, change kind `repick`) carries only the reason, and once
approved the AI picks the next task with that reason in view. The To-do tab shows
the progress to the task's planned end, notes and steps (saved at once and shown to
the AI when it reviews 「끝냈다」, "done") and the time spent on the current to-do;
today's summary, finished to-dos and every request with its answer are in the
Statistics tab. There is no plan for the whole day: the AI decides what to do now,
each time, from the life rules. The
three instructions the planner's AI follows (picking a task, reviewing a change
request, reviewing a to-do change) are settings: empty means the built-in text, and an edit is scheduled like any
looser edit. Guide (Korean): [`docs/task_planner.md`](docs/task_planner.md).

**Access requests**: from the Focus app the user can ask for an app or site
the AI check blocks, for some minutes or permanently, stating the purpose.
If the AI approves, the block is lifted, but the app stays observed and is
judged against that purpose; off-purpose use revokes the grant and blocks
again. Blocks do not expire: an approved request lifts them, or the unblock
button in the Settings tab (it lists every user's blocks; once the settings are
protected, an unblock is a scheduled update). The Settings tab also sets the
limits and can revoke grants (revoking takes effect at once); always-blocked
programs (the blacklist) and always-blocked websites are never lifted this way. Guide (Korean):
[`docs/access_grants.md`](docs/access_grants.md).

**Claude Desktop** (machine install): `digital-wellbeing-mcp.exe` is an MCP
server for Claude Desktop in the work account. Its tools show the list and the
current task, add a to-do (`propose_todo`, returning it as the AI completed
it), edit or remove one (`change_todo`), ask for a change of the task
(`request_change`: done, `repick` — the app's AI picks what comes next, neither
the user nor Claude chooses it — or pause; the former `switch_todo` and
`switch_free` are gone, though the agent API still accepts them), read the
settings (without the API key) and, once they are protected, save them (stricter
edits at once, the rest scheduled), all through the agent API the Focus app
uses, so the assistant can do nothing the user cannot. To-do changes take
effect only if the agent's AI approves them against the life rules; no tool
pushes a rejected one through. A scheduled update takes effect only after the
user confirms it three times in the Focus app; no tool answers those questions.
The Focus app's Help tab has a setup guide with a button that copies the
Claude Desktop config. Setup (Korean): [`docs/claude_desktop.md`](docs/claude_desktop.md).

**AI usage**: every Claude call's token counts are recorded per day and purpose
with an estimated cost (price table in `crates/ai_focus_rule/src/usage.rs`); the
Statistics tab shows this account's cost today against the budget and all users'
calls and cost today and this month, and an optional daily budget (Settings tab, AI
connection) pauses
focus checks (not planner or access requests) once it is spent. The API has no
way to read the remaining credit; the Claude Console shows it.

**Linux and tests**: everything but the Windows service, installer and tray runs
on Linux too; the focus monitor uses X11 there (`platform/x11.rs`; Wayland is not
supported) and the agent runs as `digital-wellbeing-service agent`.
`xvfb-run -a cargo test --workspace` includes an X11 test and an end-to-end test
that runs the real agent against real windows and a local stand-in for the Claude
API. CI (`.github/workflows/ci.yml`) runs that on Linux and, on Windows, the tests
plus a real install, upgrade (checking the data-folder ACLs) and uninstall.
Guide (Korean): [`docs/linux_and_tests.md`](docs/linux_and_tests.md).

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
apps/desktop           egui shell: rule tabs + shared process/block views (standalone only; with the service installed it only shows a notice)
crates/list_based_rule blacklist/whitelist BlockRule + RuleUi settings
crates/ai_focus_rule   AI-judged focus BlockRule + RuleUi (activity sampler, Claude judge)
crates/task_planner    AI picks the task from a to-do list; reviews change requests and to-do changes
crates/machine         machine-install paths, managed users, session ids, agent<->Focus API, settings protection (stricter/looser edits)
apps/service           Windows service, per-session agent, installer
apps/focus             Focus app for standard users (Korean UI: to-do, blocking, settings, statistics and help tabs; tray icon)
apps/mcp               MCP server for Claude Desktop (to-dos through the agent API)
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
