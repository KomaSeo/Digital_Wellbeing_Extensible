# Digital Wellbeing

**English** | [한국어](README.ko.md)

Digital Wellbeing is a Windows program that helps one person stay on the to-dos they set for themselves. This document explains what the program does, how to install it and how the repository is organized. The detailed documents are in the `docs/` folder and are written in Korean. The program's screens are also in Korean, so this README gives the Korean label in parentheses where it helps you find it.

## 1. Terms

- **To-do (할 일)**: a piece of work the user puts in the To-dos tab. It has a title, a memo, a deadline, a priority and a time estimate.
- **Current task (지금 할 일)**: the one to-do the AI picked from the list as what to do now. The AI picks it when the user presses "Get a task" (할 일 받기).
- **AI check (AI 검사)**: the AI (Claude) looks at the open windows and the screen and judges whether each window is needed for the current task.
- **Block (차단)**: closing an app or a browser tab that the AI judged to be unrelated to the current task. A block does not expire with time.
- **Access request (접근 요청)**: telling the AI what a blocked app or website is needed for. If the AI allows it, the user can use it for the time the AI sets.
- **Strict mode (엄격 모드)**: the state in which the settings are locked. Older versions called it **settings protection** (설정 보호). Before strict mode is on (the **setup stage**, 설정 단계), every settings change applies at once. After it is on, only changes that make the rules stricter apply at once; a change that loosens them applies only after it is confirmed three times, two days apart. When turning strict mode on, the user chooses how it can be turned off: after a set period, never, with administrator rights or with a password. Turning strict mode off also lifts every block.
- **Service and agent**: the service is a Windows service that runs with administrator rights (SYSTEM). It starts one agent for each signed-in account, and the agent watches and blocks that account's apps.
- **Focus app**: the window the user sees. From left to right it has the Work (일하기), To-dos (할 일), Blocks (차단), Settings (설정), Statistics (통계) and Help (도움말) tabs, and it opens on the Work tab. The Work tab shows the current task the AI picked and the change requests; the To-dos tab shows the to-do list and the appointments.

## 2. What the program does

- When the user adds to-dos, the AI looks at the life rules and the deadlines, picks the current task and decides how much time to give it.
- The AI looks at the open windows and closes apps and tabs that are unrelated to the current task. The moment it blocks one, an "Is this block right?" (맞는 차단인가요?) window appears, where the user can request access on the spot.
- When the user has finished the task, is stuck or needs more time, they send a change request, and the AI decides in light of the life rules.
- There are also always-blocked programs and websites, appointments (to-dos at a fixed time), pausing and more.
- Emergency use (비상 사용) is time when the AI checks rest. The user gives it as a daily budget, a weekly budget or fixed times of day, and uses it without the AI's approval, in strict mode too.
- A rule simulation (시뮬레이션 돌려보기) lets the user try the life rules on a virtual day before applying them: the user plays what happens (finishing early, taking longer, not doing the task, being stuck) and the AI answers as it would, with the rules being edited and the rules in force side by side.
- The program can be used by talking to it through Claude Desktop.
- An Android phone app ("DW 할 일") shows the to-do list and adds or edits to-dos and appointments. It talks to the sync server, and a PC that is on reviews the changes.
- Once strict mode is on, the program stops the user from dodging the rules by breaking the API key, not taking tasks, changing the time zone and the like.

## 3. Installing

1. Change the Windows account you work in to a standard user. If the work account is an administrator, it can undo everything this program blocks.
2. Download `DigitalWellbeing-Setup-<version>.exe` from the [releases page](https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases), run it and enter the administrator password.
3. Sign in to the work account, and the Focus app appears. In the Settings tab, enter an Anthropic API key and set your life rules and always-blocked programs. Add to-dos in the To-dos tab.
4. Use it for a few days. When it behaves the way you want, choose how strict mode can be turned off at the top of the Settings tab and turn strict mode on.

The detailed steps for installing and for preventing bypasses are in the Focus app's Help tab and in [`docs/multi_account.md`](docs/multi_account.md). What changed in this version is in [`installer/RELEASE_NOTES.md`](installer/RELEASE_NOTES.md), and what changed in older versions is on each release on the [releases page](https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases).

## 4. Documents (in Korean)

| Document | Contents |
|---|---|
| [`docs/multi_account.md`](docs/multi_account.md) | How the service and the agent are built, installing, the data folder, strict mode, preventing bypasses |
| [`docs/ai_focus_rule.md`](docs/ai_focus_rule.md) | The AI check, blocks, the block window, helper programs, locking all apps |
| [`docs/task_planner.md`](docs/task_planner.md) | To-dos, picking the current task, change requests, appointments |
| [`docs/access_grants.md`](docs/access_grants.md) | Access requests and grants |
| [`docs/claude_desktop.md`](docs/claude_desktop.md) | Using this program through Claude Desktop |
| [`docs/cost_reduction_options.md`](docs/cost_reduction_options.md) | Ways to cut AI costs and the owner's decisions |
| [`docs/linux_and_tests.md`](docs/linux_and_tests.md) | Developing and testing on Linux |
| [`docs/sync.md`](docs/sync.md) | Syncing several devices: setting up the server, pairing devices, signing in, keeping versions in step |
| [`docs/design_decisions.md`](docs/design_decisions.md) | For each feature, what the owner asked for and the details the AI chose |

## 5. Repository layout

This repository is a Rust workspace.

| Path | What it does |
|---|---|
| `apps/service` | `digital-wellbeing-service.exe`. Installing and uninstalling, the Windows service, the agent for each account |
| `apps/focus` | `digital-wellbeing-focus.exe`. The Focus app the user sees (egui) |
| `apps/mcp` | `digital-wellbeing-mcp.exe`. The MCP server Claude Desktop uses |
| `crates/enforcer` | Watches the processes and closes the programs the rules say to block, together with their helpers |
| `crates/list_based_rule` | Always-blocked programs (blacklist and whitelist) |
| `crates/ai_focus_rule` | The AI check, blocks, access grants |
| `crates/task_planner` | The to-do list and the current task, change requests and their AI review. It is connected to the AI check only through the `FocusLink` trait |
| `crates/machine` | The data folder and paths, settings and strict mode (`settings`), locking all apps (`lockdown`), signing in, sessions |
| `crates/system_care` | Work that needs administrator rights: updating winget programs, keeping the time zone, updating this program (`self_update`) |
| `crates/agent_api` | The agent's user API (`user_api`), the tools for Claude Desktop (`assistant_tools`), and `MonitorLink` (`focus_link`), which connects the planner and the AI check |
| `crates/ai_client` | Calling the Claude API, the AI connection status, token usage and cost, comparing models. It knows none of the rules |
| `crates/json_store` | A store that keeps one JSON file in memory and writes it atomically (`JsonStore`) |
| `crates/app_identity` | The names of this app's executables |
| `crates/setting_edits` | Code that splits a change to a settings file into single settings and applies them, and the frame a rule uses to describe its own settings file (`SettingsFile`) |
| `crates/browser_pages` | Browser names, reading the page and the site from a tab title, YouTube channels, search terms |
| `crates/sync` | The server and the client for syncing several devices, encryption, merging, the sign-in list |
| `installer/` | The Inno Setup install script, the release version and the release notes |
| `.github/workflows/` | `ci.yml` (tests on Linux and Windows, a real install test), `release.yml` (publishing) |

## 6. Building and testing

```sh
cargo build --workspace
cargo test --workspace                 # on Linux: xvfb-run -a cargo test --workspace
```

- On Linux the agent runs on X11, so you can develop there and run the end-to-end tests. Installing the service and the installer work only on Windows ([`docs/linux_and_tests.md`](docs/linux_and_tests.md)).
- The end-to-end tests use a fake server inside the tests instead of the Claude API. The tests that use the real API run only when the `DW_E2E_ANTHROPIC_API_KEY` environment variable is set.

## 7. Releasing

When you bump the version in `installer/RELEASE_VERSION` and push, `release.yml` builds and tests on Windows, then uploads the installer and a zip to a GitHub release. The release notes are taken as they are from `installer/RELEASE_NOTES.md`.
