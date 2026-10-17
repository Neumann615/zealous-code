# Local RC packaging with full quality gates (spec 120)
# Usage: powershell -File scripts\package-release.ps1
# Gates: clean worktree, TUI acceptance all-pass, fmt/test/clippy,
#        real_model_smoke, release build, version, config check, zip checks
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# --- toolchain PATH ---
$msysBin = Join-Path $env:USERPROFILE "msys64\mingw64\bin"
$env:Path = "$env:USERPROFILE\.cargo\bin;$msysBin;" + $env:Path

$version = "0.1.0"
$dist = Join-Path $repo "dist"
$zipName = "zealous-$version-x86_64-pc-windows-msvc.zip"
$zipPath = Join-Path $dist $zipName
$acceptanceFile = Join-Path $repo "docs\acceptance\tui-alpha.md"
$fakeKey = "rc-fake-key-DO-NOT-USE-0123456789"

function Fail {
    param([string]$Step, [string]$Detail)
    Write-Host "[FAIL] $step :: $Detail"
    exit 1
}

# --- Gate 1: clean worktree ---
$dirty = git status --porcelain
if ($dirty) { Fail "git-clean" "dirty worktree: $dirty" }
Write-Host "[gate 1/8] git clean"

# --- Gate 2: TUI acceptance record ---
if (-not (Test-Path $acceptanceFile)) {
    Fail "acceptance" "acceptance record not found: $acceptanceFile"
}
$acceptance = Get-Content $acceptanceFile -Raw
$notExecuted = ([regex]::Matches($acceptance, "\|\s*(?:未执行|待人工)")).Count
$failed = ([regex]::Matches($acceptance, "\|\s*(?:失败)")).Count
if ($notExecuted -gt 0 -or $failed -gt 0) {
    Fail "acceptance" "TUI acceptance has $notExecuted unexecuted + $failed failed items; all 8 must pass"
}
Write-Host "[gate 2/8] TUI acceptance record: all pass"

# --- Gate 3: fmt / test / clippy ---
cmd /c "cargo fmt --check"
if ($LASTEXITCODE -ne 0) { Fail "fmt" "cargo fmt --check failed" }
cmd /c "cargo test --all 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "test" "cargo test --all failed" }
Write-Host "[gate 3/8] cargo test --all"
cmd /c "cargo clippy --all-targets -- -D warnings 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "clippy" "clippy -D warnings failed" }
Write-Host "[gate 4/8] clippy -D warnings"

# --- Gate 5: real_model_smoke (explicit, consumes quota) ---
Write-Host "[gate 5/8] real_model_smoke..."
cmd /c "cargo test --test cli_e2e real_model_smoke -- --ignored --nocapture 2>&1"
if ($LASTEXITCODE -ne 0) { Fail "real-model-smoke" "real_model_smoke failed" }
Write-Host "[gate 5/8] real_model_smoke passed"

# --- Gate 6: release build ---
cmd /c "cargo build --release 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "build" "cargo build --release failed" }
$exe = Join-Path $repo "target\release\zealous.exe"
if (-not (Test-Path $exe)) { Fail "build" "missing $exe" }
Write-Host "[gate 6/8] release build"

# --- Gate 7: binary checks ---
$ver = & $exe --version
if ($ver -ne "zealous 0.1.0") { Fail "version" "unexpected: $ver" }
Write-Host "[gate 7/8] --version: $ver"

# config check: inject fake key, assert exit 0 + [ok] present + no fake key in output
$env:ZEALOUS_BASE_URL = "https://rc-check.invalid/v1"
$env:ZEALOUS_API_KEY = $fakeKey
$env:ZEALOUS_MODEL = "rc-check-model"
$cfgOut = cmd /c "`"$exe`" config check 2>&1" | Out-String
$cfgExit = $LASTEXITCODE
Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL -ErrorAction SilentlyContinue
if ($cfgExit -ne 0) { Fail "config-check" "exit=$cfgExit" }
if (-not $cfgOut.Contains("[ok]")) { Fail "config-check" "missing [ok] in output" }
if ($cfgOut.Contains($fakeKey)) { Fail "config-check" "leaked fake key" }
Write-Host "[gate 8/8] config check ok, no leak"

# --- Packaging: zip with zealous.exe only ---
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
New-Item -ItemType Directory -Path $dist -Force | Out-Null
Compress-Archive -Path $exe -DestinationPath $zipPath

# --- Zip verification: manifest + no sensitive bytes ---
$zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    if ($zip.Entries.Count -ne 1) { Fail "zip" "expected 1 entry, got $($zip.Entries.Count)" }
    $entryName = $zip.Entries[0].FullName
    if ($entryName -ne "zealous.exe") { Fail "zip" "unexpected entry: $entryName" }
    # Scan raw bytes for fake key
    $entry = $zip.Entries[0]
    $stream = $entry.Open()
    $ms = New-Object System.IO.MemoryStream
    $stream.CopyTo($ms)
    $bytes = $ms.ToArray()
    $ms.Dispose(); $stream.Dispose()
    $text = [System.Text.Encoding]::ASCII.GetString($bytes)
    if ($text.Contains($fakeKey)) { Fail "zip" "leaked fake key in binary" }
} finally {
    $zip.Dispose()
}
Write-Host "[ok] zip manifest: zealous.exe only, no sensitive bytes"

# --- SHA256 ---
$sha256 = (Get-FileHash $zipPath -Algorithm SHA256).Hash
$size = (Get-Item $zipPath).Length
Write-Host ""
Write-Host "=== RC Ready ==="
Write-Host "Path:   $zipPath"
Write-Host "Size:   $size bytes"
Write-Host "SHA256: $sha256"
Write-Host ""
Write-Host "Record these in docs/acceptance/rc-0.1.0.md"
