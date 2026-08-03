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
| `add_whitelist` | `{ "name": "code.exe" }` | Add to whitelist |
| `remove_whitelist` | `{ "name": "code.exe" }` | Remove (protected names rejected) |
| `list_processes` | `{}` | Running process snapshot |
| `list_blocks` | `{ "limit": 50 }` | Recent terminations |
| `save_strategy` | `{ "name": "focus" }` | Snapshot current lists as a named strategy |
| `apply_strategy` | `{ "name": "focus" }` | Load a saved strategy |

### Example (PowerShell helper)

```powershell
# App must be running first
.\scripts\call.ps1 -Method ping
.\scripts\call.ps1 -Method set_mode -Params @{ mode = "blacklist" }
.\scripts\call.ps1 -Method add_blacklist -Params @{ name = "steam.exe" }
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
apps/desktop     egui desktop shell + enforcer host
crates/policy    blacklist/whitelist model + persistence
crates/enforcer  process reconcile / terminate loop
crates/tool-api  JSON-RPC function-call router
scripts/call.ps1 convenience client
```

Browser extension support is intentionally deferred; keep policy logic in `crates/policy` so a future browser host can reuse the same rules.

## Safety notes

- Critical Windows processes (`csrss.exe`, `winlogon.exe`, `explorer.exe`, …) are never killed.
- **Whitelist mode is aggressive** — only allowlisted apps (plus protected system processes) may run.
- Changes via function call are written to disk immediately and applied on the next enforcer tick (~0.8s).
