<#
Responsibility: Rebuild and reinstall the machine install from this checkout in
one step, for testing on your own Windows PC (much faster than a CI release:
after the first build only changed crates are recompiled).

Usage, from an administrator PowerShell in the repository folder:
  .\scripts\dev-install.ps1 -Manage work   # first time: name the work account
  .\scripts\dev-install.ps1                # later: keeps the managed accounts
  .\scripts\dev-install.ps1 -Tail          # then follow the newest agent log

If scripts are blocked, run once: Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
#>
param(
    [string]$Manage,
    [switch]$Tail
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Error "Run this from an administrator PowerShell."
}

Write-Host "== build (release) ==" -ForegroundColor Cyan
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "== install ==" -ForegroundColor Cyan
$installArgs = @("install")
if ($Manage) { $installArgs += @("--manage", $Manage) }
& "$repo\target\release\digital-wellbeing-service.exe" @installArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "== service ==" -ForegroundColor Cyan
Get-Service DigitalWellbeing | Format-Table -AutoSize Name, Status, StartType

$logs = Join-Path $env:ProgramData "DigitalWellbeing\logs"
Write-Host "== logs in $logs ==" -ForegroundColor Cyan
Start-Sleep -Seconds 3   # give the service a moment to start the agents
$service = Join-Path $logs "service.log"
if (Test-Path $service) {
    Write-Host "-- service.log (last 10 lines)"
    Get-Content $service -Tail 10
}
$agent = Get-ChildItem $logs -Filter "agent-session*.log" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($agent) {
    Write-Host "-- $($agent.Name) (last 15 lines)"
    Get-Content $agent.FullName -Tail 15
} else {
    Write-Host "No agent log yet: log in to the managed account (or check service.log)."
}

if ($Tail) {
    while (-not $agent) {
        Start-Sleep -Seconds 2
        $agent = Get-ChildItem $logs -Filter "agent-session*.log" -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
    }
    Write-Host "== following $($agent.Name) (Ctrl+C to stop) ==" -ForegroundColor Cyan
    Get-Content $agent.FullName -Tail 0 -Wait
}
