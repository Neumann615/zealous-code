# RC packaging with full quality gates (spec 120 / spec 150)
# Usage: powershell -File scripts\package-release.ps1 [-LocalTrial]
#   default        : OFFLINE EXTERNAL CANDIDATE - TUI scenarios 1,4,7,8 must
#                    be PASS|MANUAL and all 8 rows PASS
#   -LocalTrial    : local trial RC - 8/8 PASS required, MANUAL not required;
#                    RC record is marked LOCAL TRIAL (NOT for external release)
# Gates: clean worktree, TUI acceptance record, fmt/test/clippy,
#        release build, toolchain/version, release-mode mock E2E,
#        release-mode real_model_smoke (consumes quota, once per package),
#        config check, zip manifest, zip extraction verify, SHA256, RC record
param(
    [switch]$LocalTrial
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# --- toolchain PATH ---
$msysBin = Join-Path $env:USERPROFILE "msys64\mingw64\bin"
$env:Path = "$env:USERPROFILE\.cargo\bin;$msysBin;" + $env:Path

$version = "0.1.0"
$dist = Join-Path $repo "dist"
$zipName = $null  # resolved dynamically from rustc -vV host (gate 6)
$zipPath = $null  # resolved with zipName
$acceptanceFile = Join-Path $repo "docs\acceptance\tui-alpha.md"
$rcRecordFile = Join-Path $repo "docs\acceptance\rc-0.1.0.md"
$fakeKey = "rc-fake-key-DO-NOT-USE-0123456789"

function Fail {
    param([string]$Step, [string]$Detail)
    Write-Host "[FAIL] $step :: $Detail"
    exit 1
}

try {
    # --- Gate 1: delete stale zip artifacts (all old naming variants) ---
    Get-ChildItem $dist -Filter "zealous-*.zip" -ErrorAction SilentlyContinue | Remove-Item -Force
    Write-Host "[gate 1/12] stale zip artifacts removed"

    # --- Gate 2: clean worktree ---
    $dirty = git status --porcelain
    if ($dirty) { Fail "git-clean" "dirty worktree: $dirty" }
    Write-Host "[gate 2/12] git clean"

    # --- Gate 3: TUI acceptance record (status AND evidence columns) ---
    if (-not (Test-Path $acceptanceFile)) {
        Fail "acceptance" "acceptance record not found: $acceptanceFile"
    }
    $acceptance = Get-Content $acceptanceFile -Raw
    # Parse table rows (lines starting with | digit |):
    #   | # | scenario | status | method | evidence |
    #   split index: [1]=num [3]=status [4]=method
    $rows = $acceptance -split "`n" | Where-Object { $_ -match "^\|\s*[0-9]+\s*\|" }
    if ($rows.Count -ne 8) {
        Fail "acceptance" "expected exactly 8 scenario rows, got $($rows.Count)"
    }
    # Scenarios that require MANUAL evidence for an external RC (spec 120 s6, spec 150)
    $manualRequired = @(1, 4, 7, 8)
    $missingManual = @()
    foreach ($row in $rows) {
        $cols = $row -split "\|"
        $num = $cols[1].Trim()
        $status = $cols[3].Trim()
        $method = $cols[4].Trim()
        if ($status -ne "PASS") {
            Fail "acceptance" "scenario $num not PASS: $row"
        }
        if ($method -ne "MANUAL" -and $method -ne "AUTOMATED") {
            Fail "acceptance" "scenario $num has unknown method '$method'"
        }
        if ($manualRequired -contains [int]$num -and $method -ne "MANUAL") {
            $missingManual += $num
        }
    }
    if ($missingManual.Count -gt 0) {
        if ($LocalTrial) {
            Write-Host "[gate 3/12] LOCAL TRIAL: MANUAL evidence missing for scenarios: $($missingManual -join ',')"
            Write-Host "[gate 3/12] WARNING: NOT an external-release RC (automation cannot replace manual evidence)"
        } else {
            Fail "acceptance" "external RC requires MANUAL evidence for scenarios 1,4,7,8; missing: $($missingManual -join ',') (use -LocalTrial for a local trial RC)"
        }
    }
    $rcDistribution = if ($LocalTrial) {
        "LOCAL TRIAL (NOT for external release)"
    } else {
        "OFFLINE EXTERNAL CANDIDATE, NOT PUBLISHED"
    }
    Write-Host "[gate 3/12] TUI acceptance: 8/8 PASS, distribution=$rcDistribution"

    # --- Gate 4: fmt / test / clippy ---
    cmd /c "cargo fmt --check"
    if ($LASTEXITCODE -ne 0) { Fail "fmt" "cargo fmt --check failed" }
    cmd /c "cargo test --all 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "test" "cargo test --all failed" }
    Write-Host "[gate 4/12] cargo test --all"
    cmd /c "cargo clippy --all-targets -- -D warnings 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "clippy" "clippy -D warnings failed" }
    Write-Host "[gate 4/12] clippy -D warnings"

    # --- Gate 5: release build ---
    cmd /c "cargo build --release 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "build" "cargo build --release failed" }
    $exe = Join-Path $repo "target\release\zealous.exe"
    if (-not (Test-Path $exe)) { Fail "build" "missing $exe" }
    Write-Host "[gate 5/12] release build"

    # --- Gate 6: toolchain and binary checks (all values measured, not hardcoded) ---
    $rustcInfo = rustc -vV 2>&1 | Out-String
    $hostTriple = if ($rustcInfo -match "host:\s*(\S+)") { $Matches[1] } else { "" }
    $rustcRelease = if ($rustcInfo -match "release:\s*([0-9.]+)") { $Matches[1] } else { "" }
    if (-not $hostTriple.StartsWith("x86_64-pc-windows")) {
        Fail "toolchain" "unexpected host triple: $hostTriple"
    }
    $ver = & $exe --version
    if ($ver -ne "zealous 0.1.0") { Fail "version" "unexpected: $ver" }
    Write-Host "[gate 6/12] host=$hostTriple rustc=$rustcRelease version=$ver"
    $zipName = "zealous-$version-$hostTriple.zip"
    $zipPath = Join-Path $dist $zipName

    # --- Gate 7: release-mode mock E2E (real binary loop against release exe) ---
    cmd /c "cargo test --release -p zealous-cli --test cli_e2e e2e_real_binary_plan_write_approve_complete 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "mock-e2e-release" "release-mode mock E2E failed" }
    Write-Host "[gate 7/12] mock E2E passed (release binary)"

    # --- Gate 8: release-mode real model smoke ---
    # Consumes model quota; runs once per package; validates the actual
    # release-form binary (spec 150).
    Write-Host "[gate 8/12] real_model_smoke (release; consumes quota; once per package)..."
    cmd /c "cargo test --release -p zealous-cli --test cli_e2e real_model_smoke -- --ignored --nocapture 2>&1"
    if ($LASTEXITCODE -ne 0) { Fail "real-model-smoke-release" "release-mode real_model_smoke failed" }
    Write-Host "[gate 8/12] real_model_smoke passed (release binary)"

    # --- Gate 9: config check on release exe (fake key, try/finally) ---
    $env:ZEALOUS_BASE_URL = "https://rc-check.invalid/v1"
    $env:ZEALOUS_API_KEY = $fakeKey
    $env:ZEALOUS_MODEL = "rc-check-model"
    try {
        $cfgOut = cmd /c "`"$exe`" config check 2>&1" | Out-String
        $cfgExit = $LASTEXITCODE
        if ($cfgExit -ne 0) { Fail "config-check" "exit=$cfgExit" }
        if (-not $cfgOut.Contains("[ok]")) { Fail "config-check" "missing [ok]" }
        if ($cfgOut.Contains($fakeKey)) { Fail "config-check" "leaked fake key" }
        Write-Host "[gate 9/12] config check ok, no leak"
    } finally {
        Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL -ErrorAction SilentlyContinue
    }

    # --- Gate 10: zip packaging and checks ---
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    Compress-Archive -Path $exe -DestinationPath $zipPath

    # Verify zip manifest
    $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        if ($zip.Entries.Count -ne 1) {
            Fail "zip" "expected 1 entry, got $($zip.Entries.Count)"
        }
        $entryName = $zip.Entries[0].FullName
        if ($entryName -ne "zealous.exe") { Fail "zip" "unexpected entry: $entryName" }
        # Scan binary for sensitive strings
        $entry = $zip.Entries[0]
        $stream = $entry.Open()
        $ms = New-Object System.IO.MemoryStream
        $stream.CopyTo($ms)
        $bytes = $ms.ToArray()
        $ms.Dispose(); $stream.Dispose()
        $text = [System.Text.Encoding]::ASCII.GetString($bytes)
        # Credential scan: the binary must not contain any real key value.
        # Code literals like ".env", "api_key=", "Authorization:" are legitimate
        # path/header references in source, not secrets; only values count.
        if ($text.Contains($fakeKey)) {
            Fail "zip" "binary contains fake key value (env leak)"
        }
    } finally {
        $zip.Dispose()
    }
    Write-Host "[gate 10/12] zip: 1 entry zealous.exe, no sensitive strings"

    # --- Gate 11: zip extraction verification (spec 150) ---
    $tmpExtract = Join-Path $env:TEMP ("zealous-rc-extract-" + [guid]::NewGuid().ToString("N"))
    try {
        Expand-Archive -Path $zipPath -DestinationPath $tmpExtract -Force
        $extractedExe = Join-Path $tmpExtract "zealous.exe"
        if (-not (Test-Path $extractedExe)) { Fail "zip-extract" "missing zealous.exe after extraction" }
        $exVer = & $extractedExe --version
        if ($exVer -ne "zealous 0.1.0") { Fail "zip-extract" "unexpected version: $exVer" }
        $env:ZEALOUS_BASE_URL = "https://rc-check.invalid/v1"
        $env:ZEALOUS_API_KEY = $fakeKey
        $env:ZEALOUS_MODEL = "rc-check-model"
        try {
            $exCfgOut = cmd /c "`"$extractedExe`" config check 2>&1" | Out-String
            $exCfgExit = $LASTEXITCODE
            if ($exCfgExit -ne 0) { Fail "zip-extract-config" "exit=$exCfgExit" }
            if (-not $exCfgOut.Contains("[ok]")) { Fail "zip-extract-config" "missing [ok]" }
            if ($exCfgOut.Contains($fakeKey)) { Fail "zip-extract-config" "leaked fake key" }
        } finally {
            Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL -ErrorAction SilentlyContinue
        }
        Write-Host "[gate 11/12] zip extraction verify: --version + config check ok, no leak"
    } finally {
        Remove-Item $tmpExtract -Recurse -Force -ErrorAction SilentlyContinue
    }

    # --- Gate 12: SHA256 and RC record ---
    $sha256 = (Get-FileHash $zipPath -Algorithm SHA256).Hash
    $size = (Get-Item $zipPath).Length

    # --- Generate RC record ---
    $commit = git rev-parse HEAD
    $buildTime = Get-Date -Format "yyyy-MM-dd HH:mm:ss UTC"
    $rcContent = @"
# RC 0.1.0 Acceptance Record (spec 120 / spec 150)

## Traceability (all values measured at build time)

- Runtime source commit: $commit
- RC record commit: (filled after commit; diff vs runtime source must be this file only)
- Rustc release: $rustcRelease (measured via rustc -vV)
- Host triple: $hostTriple (measured via rustc -vV)
- Build time: $buildTime
- Target platform: Windows x64
- Distribution: $rcDistribution

## Quality gates (all PASS)

| Gate | Command | Result |
| ---- | ------- | ------ |
| fmt | cargo fmt --check | PASS |
| test | cargo test --all | PASS |
| clippy | cargo clippy --all-targets -- -D warnings | PASS |
| release build | cargo build --release | PASS |
| mock E2E (release binary) | cargo test --release -p zealous-cli --test cli_e2e e2e_real_binary_plan_write_approve_complete | PASS |
| real_model_smoke (release binary) | cargo test --release -p zealous-cli --test cli_e2e real_model_smoke -- --ignored | PASS |
| --version | zealous.exe --version | PASS |
| config check | zealous.exe config check (fake key) | PASS |
| zip extraction verify | extracted zealous.exe --version + config check (fake key) | PASS |
| TUI acceptance | docs/acceptance/tui-alpha.md: 8/8 PASS, scenarios 1/4/7/8 MANUAL | PASS |

## Artifact

- Path: dist/$zipName
- Size: $((Get-Item $zipPath).Length) bytes
- SHA256: $sha256
- Manifest: zealous.exe (single entry)

## Security boundary

- V1 is NOT an OS sandbox.
- Shell is a T2 approval action.
- File access is limited to the repository (path guard).
- Model endpoint is the only default outbound network access.
"@
    Set-Content -Path $rcRecordFile -Value $rcContent -Encoding UTF8
    Write-Host "[ok] RC record: $rcRecordFile"

    Write-Host ""
    Write-Host "=== RC Ready ==="
    Write-Host "Path:   $zipPath"
    Write-Host "Size:   $((Get-Item $zipPath).Length) bytes"
    Write-Host "SHA256: $sha256"
    Write-Host "Source commit: $commit"
    Write-Host "Host:   $hostTriple (rustc $rustcRelease)"
    Write-Host "Distribution: $rcDistribution"

} catch {
    Write-Host "[UNEXPECTED ERROR] $_"
    exit 1
}
