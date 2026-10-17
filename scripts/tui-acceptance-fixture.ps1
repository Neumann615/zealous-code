# TUI manual acceptance fixture (spec 90 s5 / 110 closeout)
# Usage: powershell -File scripts\tui-acceptance-fixture.ps1
# Creates 4 repos under %TEMP%\zealous-tui-acceptance\ for the 8 scenarios.
# ASCII-only to avoid PowerShell encoding issues.
$ErrorActionPreference = "Stop"

# --- self checks ---
$exe = Join-Path $PSScriptRoot "..\target\debug\zealous.exe"
if (-not (Test-Path $exe)) {
    Write-Host "[FAIL] zealous.exe not found at $exe"
    Write-Host "       Run: cargo build -p zealous-cli"
    exit 1
}
Write-Host "[ok] zealous.exe: $exe"

try {
    $null = git --version
    Write-Host "[ok] git available"
} catch {
    Write-Host "[FAIL] git not available"
    exit 1
}

$base = Join-Path $env:TEMP "zealous-tui-acceptance"
if (Test-Path $base) {
    # safety: only clean if under TEMP
    $tempRoot = $env:TEMP
    if (-not $base.StartsWith($tempRoot)) {
        Write-Host "[FAIL] $base is not under $tempRoot"
        exit 1
    }
    Remove-Item -Recurse -Force $base
}
New-Item -ItemType Directory -Path $base | Out-Null
Write-Host "[ok] base dir: $base"

function New-Repo {
    param([string]$Dir)
    New-Item -ItemType Directory -Path $Dir | Out-Null
    git -C $Dir init -q
    Set-Content -Path (Join-Path $Dir "README.md") -Value "fixture`n"
    git -C $Dir add -A
    git -C $Dir -c user.name=t -c user.email=t@t commit -q -m init
}

# --- repo 1: normal task + brief file (scenarios 1-3, 5-7) ---
$main = Join-Path $base "main"
New-Repo $main
Set-Content -Path (Join-Path $main "BRIEF.md") -Value "Replace the word fixture with ACCEPTED in README.md. Then report the new content."
Write-Host "[main] $main (scenarios 1-3, 5-7: render / brief / real model / interrupt / resume / exit)"

# --- repo 2: large edit_file approval (scenario 4) ---
$big = Join-Path $base "big-edit"
New-Repo $big
$bigContent = ("A" * 40 + "`n") * 200
Set-Content -Path (Join-Path $big "big.txt") -Value $bigContent
git -C $big add -A
git -C $big -c user.name=t -c user.email=t@t commit -q -m init
# brief: replace a large block
$briefContent = ("B" * 40 + "`n") * 100
Set-Content -Path (Join-Path $big "EDIT-BRIEF.md") -Value $briefContent
Write-Host "[big-edit] $big (scenario 4: large edit_file approval + scroll)"

# --- repo 3: corrupted middle ledger (scenario 8) ---
$corrupt = Join-Path $base "corrupt"
New-Repo $corrupt
Set-Content -Path (Join-Path $corrupt "README.md") -Value "corrupt fixture`n"
git -C $corrupt add -A
git -C $corrupt -c user.name=t -c user.email=t@t commit -q -m init
# Use CLI to create a real task with real ledger
$env:ZEALOUS_BASE_URL = "https://rc.invalid/v1"
$env:ZEALOUS_API_KEY = "fake"
$env:ZEALOUS_MODEL = "fake"
$null = & $exe task new "corrupt ledger test" 2>$null
$taskDirs = Get-ChildItem (Join-Path $corrupt ".zealous\tasks") -Directory -ErrorAction SilentlyContinue
if ($taskDirs) {
    $ledger = Get-ChildItem $taskDirs[0].FullName -Recurse -Filter events.jsonl | Select-Object -First 1
    if ($ledger) {
        $lines = [System.IO.File]::ReadAllLines($ledger.FullName)
        $lines[0] = "{{{ corrupted"
        [System.IO.File]::WriteAllLines($ledger.FullName, $lines)
        Write-Host "[corrupt] ledger middle line corrupted: $($taskDirs[0].FullName)"
    }
}
Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL -ErrorAction SilentlyContinue

# --- repo 4: torn tail ledger (scenario 8 variant) ---
$torn = Join-Path $base "torn"
New-Repo $torn
Set-Content -Path (Join-Path $torn "README.md") -Value "torn fixture`n"
git -C $torn add -A
git -C $torn -c user.name=t -c user.email=t@t commit -q -m init
$null = & $exe task new "torn tail test" 2>$null
$tornTasks = Get-ChildItem (Join-Path $torn ".zealous\tasks") -Directory -ErrorAction SilentlyContinue
if ($tornTasks) {
    $tornLedger = Get-ChildItem $tornTasks[0].FullName -Recurse -Filter events.jsonl | Select-Object -First 1
    if ($tornLedger) {
        [System.IO.File]::AppendAllText($tornLedger.FullName, '{"seq":99,"ts":"2026')
        Write-Host "[torn] torn tail appended: $($tornLedger.FullName)"
    }
}

Write-Host ""
Write-Host "=== Acceptance repos ready ==="
Write-Host "Open Windows Terminal, then:"
Write-Host "  cd $main"
Write-Host "  $exe tui"
Write-Host ""
Write-Host "Follow docs/acceptance/tui-alpha.md for the 8 scenarios."
Write-Host "Scenario 8 (corrupt ledger): cd $corrupt then run tui again."
