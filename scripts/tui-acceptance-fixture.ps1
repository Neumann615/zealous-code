# TUI manual acceptance fixture (spec 90 s5 / 110 closeout)
# Usage: powershell -File scripts\tui-acceptance-fixture.ps1
# Creates 4 repos under %TEMP%\zealous-tui-acceptance\ for the 8 scenarios.
# All ASCII to avoid Windows PowerShell encoding issues.
$ErrorActionPreference = "Stop"

# --- self checks ---
$exe = Join-Path $PSScriptRoot "..\target\debug\zealous.exe"
if (-not (Test-Path $exe)) {
    Write-Host "[FAIL] zealous.exe not found at $exe"
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
    $resolvedBase = [System.IO.Path]::GetFullPath($base)
    $resolvedTemp = [System.IO.Path]::GetFullPath($env:TEMP)
    if (-not $resolvedBase.StartsWith($resolvedTemp)) {
        Write-Host "[FAIL] $base is not under $env:TEMP - refusing to clean"
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

function Add-FileAndCommit {
    param([string]$Dir, [string]$File, [string]$Content)
    Set-Content -Path (Join-Path $Dir $File) -Value $Content
    git -C $Dir add -A
    git -C $Dir -c user.name=t -c user.email=t@t commit -q -m $File
}

# --- repo 1: normal task + brief file (scenarios 1-3, 5-7) ---
$main = Join-Path $base "main"
New-Repo $main
Add-FileAndCommit $main "BRIEF.md" "Replace fixture with ACCEPTED in README.md. Then report the new content."
Write-Host "[main] $main (scenarios 1-3, 5-7)"
Write-Host "  Brief file: BRIEF.md (already committed, worktree clean)"

# --- repo 2: large edit_file approval (scenario 4) ---
$bigEdit = Join-Path $base "big-edit"
New-Repo $bigEdit
$bigContent = ("lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor " * 80) + "`n"
Set-Content -Path (Join-Path $bigEdit "big.txt") -Value $bigContent
Add-FileAndCommit $bigEdit "EDIT-BRIEF.md" ("Replace all 'lorem ipsum' with 'MODIFIED-BY-MODEL'. " * 30) + "Report line count before and after."
Write-Host "[big-edit] $bigEdit (scenario 4: large edit_file + scroll)"

# --- repo 3: corrupted middle ledger (scenario 8) ---
$corrupt = Join-Path $base "corrupt"
New-Repo $corrupt
Set-Content -Path (Join-Path $corrupt "README.md") -Value "corrupt fixture`n"
git -C $corrupt add -A
git -C $corrupt -c user.name=t -c user.email=t@t commit -q -m init
# Use CLI to create real tasks (generates real ledgers)
Push-Location $corrupt
try {
    & $exe task new "corrupt: middle broken" 2>$null | Out-Null
    & $exe task new "corrupt: torn tail" 2>$null | Out-Null
} finally {
    Pop-Location
}
# Corrupt middle: change line 2 to invalid JSON (line 1 = TaskCreated stays valid)
$corruptTasks = Get-ChildItem (Join-Path $corrupt ".zealous\tasks") -Directory
foreach ($td in $corruptTasks) {
    $ledger = Get-ChildItem $td.FullName -Recurse -Filter events.jsonl | Select-Object -First 1
    if ($ledger) {
        $allLines = [System.IO.File]::ReadAllLines($ledger.FullName)
        if ($allLines.Count -ge 2) {
            $allLines[1] = "{{{ corrupted middle"
            [System.IO.File]::WriteAllLines($ledger.FullName, $allLines)
            Write-Host "[corrupt] middle line corrupted in $($td.Name)"
        }
    }
}
Write-Host "[corrupt] $corrupt (scenario 8: corrupted ledger)"

# --- repo 4: torn tail ledger (scenario 8 variant) ---
$torn = Join-Path $base "torn"
New-Repo $torn
Set-Content -Path (Join-Path $torn "README.md") -Value "torn fixture`n"
git -C $torn add -A
git -C $torn -c user.name=t -c user.email=t@t commit -q -m init
Push-Location $torn
try {
    & $exe task new "torn tail test" 2>$null | Out-Null
} finally {
    Pop-Location
}
# Append torn tail (partial JSON without newline)
$tornTasks = Get-ChildItem (Join-Path $torn ".zealous\tasks") -Directory
foreach ($td in $tornTasks) {
    $ledger = Get-ChildItem $td.FullName -Recurse -Filter events.jsonl | Select-Object -First 1
    if ($ledger) {
        [System.IO.File]::AppendAllText($ledger.FullName, '{"seq":99,"ts":"2026')
        Write-Host "[torn] torn tail appended: $($ledger.FullName)"
    }
}

# --- verification ---
Write-Host ""
Write-Host "=== Verification ==="
$allOk = $true
foreach ($r in @($main, $bigEdit, $corrupt, $torn)) {
    if (-not (Test-Path $r)) {
        Write-Host "[FAIL] missing repo: $r"
        $allOk = $false
    }
}
$corruptLedgers = Get-ChildItem (Join-Path $corrupt ".zealous\tasks") -Recurse -Filter events.jsonl -ErrorAction SilentlyContinue
if ($corruptLedgers.Count -lt 2) {
    Write-Host "[FAIL] corrupt repo should have 2 ledgers"
    $allOk = $false
}
$tornLedgers = Get-ChildItem (Join-Path $torn ".zealous\tasks") -Recurse -Filter events.jsonl -ErrorAction SilentlyContinue
if ($tornLedgers.Count -lt 1) {
    Write-Host "[FAIL] torn repo should have at least 1 ledger"
    $allOk = $false
}
if ($allOk) {
    Write-Host "[ok] all 4 repos verified"
} else {
    exit 1
}

Write-Host ""
Write-Host "=== Acceptance repos ready ==="
Write-Host "main:     $main (scenarios 1-3, 5-7)"
Write-Host "big-edit: $bigEdit (scenario 4)"
Write-Host "corrupt:  $corrupt (scenario 8: corrupted middle)"
Write-Host "torn:     $torn (scenario 8: torn tail)"
Write-Host ""
Write-Host "Open Windows Terminal, then:"
Write-Host "  cd $main"
Write-Host "  $exe tui"
Write-Host ""
Write-Host "Follow docs/acceptance/tui-alpha.md for the 8 scenarios."
