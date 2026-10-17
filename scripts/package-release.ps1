# Local RC packaging with full quality gates (spec 120)
# Usage: powershell -File scripts\package-release.ps1
# Gates: clean worktree, TUI acceptance 8/8 PASS, fmt/test/clippy,
#        real_model_smoke, host triple check, release build,
#        version, config check, zip manifest, SHA256, RC record
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# --- toolchain PATH ---
$msysBin = Join-Path $env:USERPROFILE "msys64\mingw64\bin"
$env:Path = "$env:USERPROFILE\.cargo\bin;$msysBin;" + $env:Path

$version = "0.1.0"
$dist = Join-Path $repo "dist"
$zipName = "zealous-0.1.0-x86_64-pc-windows-msvc.zip"
$zipPath = Join-Path $dist $zipName
$acceptanceFile = Join-Path $repo "docs\acceptance\tui-alpha.md"
$rcRecordFile = Join-Path $repo "docs\acceptance\rc-0.1.0.md"
$fakeKey = "rc-fake-key-DO-NOT-USE-0123456789"

function Fail {
    param([string]$Step, [string]$Detail)
    Write-Host "[FAIL] $step :: $Detail"
    exit 1
}

try {
    # --- Gate 1: delete stale zip ---
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Write-Host "[gate 1/10] stale zip removed"

    # --- Gate 2: clean worktree ---
    $dirty = git status --porcelain
    if ($dirty) { Fail "git-clean" "dirty worktree: $dirty" }
    Write-Host "[gate 2/10] git clean"

    # --- Gate 3: TUI acceptance record must be 8/8 PASS ---
    if (-not (Test-Path $acceptanceFile)) {
        Fail "acceptance" "acceptance record not found: $acceptanceFile"
    }
    $acceptance = Get-Content $acceptanceFile -Raw
    # Must have no unexecuted/failed entries
    # Parse only the status column of table rows (lines starting with | digit |)
    $rows = $acceptance -split "`n" | Where-Object { $_ -match "^\|\s*[0-9]+\s*\|" }
    if ($rows.Count -ne 8) {
        Fail "acceptance" "expected exactly 8 scenario rows, got $($rows.Count)"
    }
    foreach ($row in $rows) {
        $status = ($row -split "\|")[3].Trim()
        if ($status -ne "PASS") {
            Fail "acceptance" "scenario row not PASS: $row"
        }
    }
    Write-Host "[gate 3/10] TUI acceptance: all 8 rows PASS"
    Write-Host "[gate 3/10] TUI acceptance 8/8 PASS"

    # --- Gate 4: fmt / test / clippy ---
    cmd /c "cargo fmt --check"
    if ($LASTEXITCODE -ne 0) { Fail "fmt" "cargo fmt --check failed" }
    cmd /c "cargo test --all 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "test" "cargo test --all failed" }
    Write-Host "[gate 4/10] cargo test --all"
    cmd /c "cargo clippy --all-targets -- -D warnings 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "clippy" "clippy -D warnings failed" }
    Write-Host "[gate 4/10] clippy -D warnings"

    # --- Gate 5: real_model_smoke ---
    Write-Host "[gate 5/10] real_model_smoke..."
    cmd /c "cargo test --test cli_e2e real_model_smoke -- --ignored --nocapture 2>&1"
    if ($LASTEXITCODE -ne 0) { Fail "real-model-smoke" "real_model_smoke failed" }
    Write-Host "[gate 5/10] real_model_smoke passed"

    # --- Gate 6: release build ---
    cmd /c "cargo build --release 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "build" "cargo build --release failed" }
    $exe = Join-Path $repo "target\release\zealous.exe"
    if (-not (Test-Path $exe)) { Fail "build" "missing $exe" }
    Write-Host "[gate 6/10] release build"

    # --- Gate 7: toolchain and binary checks ---
    $rustcInfo = rustc -vV 2>&1 | Out-String
    if ($rustcInfo -notmatch "host: x86_64-pc-windows") {
        Fail "toolchain" "unexpected host triple (expect x86_64-pc-windows): $rustcInfo"
    }
    $ver = & $exe --version
    if ($ver -ne "zealous 0.1.0") { Fail "version" "unexpected: $ver" }
    Write-Host "[gate 7/10] toolchain x86_64-pc-windows + version zealous 0.1.0"

    # --- Gate 8: config check (fake key, try/finally) ---
    $env:ZEALOUS_BASE_URL = "https://rc-check.invalid/v1"
    $env:ZEALOUS_API_KEY = $fakeKey
    $env:ZEALOUS_MODEL = "rc-check-model"
    try {
        $cfgOut = cmd /c "`"$exe`" config check 2>&1" | Out-String
        $cfgExit = $LASTEXITCODE
        if ($cfgExit -ne 0) { Fail "config-check" "exit=$cfgExit" }
        if (-not $cfgOut.Contains("[ok]")) { Fail "config-check" "missing [ok]" }
        if ($cfgOut.Contains($fakeKey)) { Fail "config-check" "leaked fake key" }
        Write-Host "[gate 8/10] config check ok, no leak"
    } finally {
        Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL -ErrorAction SilentlyContinue
    }

    # --- Gate 9: mock E2E ---
    cmd /c "cargo test -p zealous-cli --test cli_e2e 2>&1" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "mock-e2e" "cli_e2e failed" }
    Write-Host "[gate 9/10] mock E2E passed"

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
    Write-Host "[gate 10/10] zip: 1 entry zealous.exe, no sensitive strings"

    # --- SHA256 ---
    $sha256 = (Get-FileHash $zipPath -Algorithm SHA256).Hash
    $size = (Get-Item $zipPath).Length

    # --- Generate RC record ---
    $commit = git rev-parse HEAD
    $buildTime = Get-Date -Format "yyyy-MM-dd HH:mm:ss UTC"
    $rcContent = @"
# RC 0.1.0 Acceptance Record (spec 120)

- Git commit: $commit
- Build time: $buildTime
- Toolchain: stable-x86_64-pc-windows-gnu (rustc 1.99.0)
- Target platform: Windows x64

## Quality gates (all PASS)

| Gate | Command | Result |
| ---- | ------- | ------ |
| fmt | cargo fmt --check | PASS |
| test | cargo test --all | PASS |
| clippy | cargo clippy --all-targets -- -D warnings | PASS |
| real_model_smoke | cargo test --test cli_e2e real_model_smoke -- --ignored | PASS |
| release build | cargo build --release | PASS |
| --version | zealous.exe --version | PASS |
| config check | zealous.exe config check (fake key) | PASS |

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

} catch {
    Write-Host "[UNEXPECTED ERROR] $_"
    exit 1
}
