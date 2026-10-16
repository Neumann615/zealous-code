# Local release candidate packaging (spec 120)
# Usage: powershell -File scripts\package-release.ps1
# Output: dist\zealous-0.1.0-x86_64-pc-windows-msvc.zip (zealous.exe only)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
# toolchain: stable-gnu needs msys64 dlltool (windows-sys import libs)
$msysBin = Join-Path $env:USERPROFILE "msys64\mingw64\bin"
$env:Path = "$env:USERPROFILE\.cargo\bin;$msysBin;" + $env:Path

$version = "0.1.0"
$target = "x86_64-pc-windows-msvc"
$dist = Join-Path $repo "dist"
$zipName = "zealous-$version-$target.zip"
$zipPath = Join-Path $dist $zipName

function Fail {
    param([string]$Step, [string]$Detail)
    Write-Host "[FAIL] $step :: $Detail"
    exit 1
}

# 1) clean worktree
$dirty = git status --porcelain
if ($dirty) { Fail "git-clean" "dirty worktree: $dirty" }
Write-Host "[ok] git clean"

# 2) fmt / test / clippy
cmd /c "cargo fmt --check"
if ($LASTEXITCODE -ne 0) { Fail "fmt" "cargo fmt --check failed" }
cmd /c "cargo test --all 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "test" "cargo test --all failed" }
Write-Host "[ok] cargo test --all"
cmd /c "cargo clippy --all-targets -- -D warnings 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "clippy" "clippy -D warnings failed" }
Write-Host "[ok] clippy -D warnings"

# 3) release build
cmd /c "cargo build --release 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "build" "cargo build --release failed" }
$exe = Join-Path $repo "target\release\zealous.exe"
if (-not (Test-Path $exe)) { Fail "build" "missing $exe" }
Write-Host "[ok] release build"

# 4) --version
$ver = & $exe --version
if ($ver -notmatch "zealous $version") { Fail "version" "unexpected: $ver" }
Write-Host "[ok] version: $ver"

# 5) config check must not leak the (fake) key
$fakeKey = "rc-fake-key-DO-NOT-USE-0123456789"
$env:ZEALOUS_BASE_URL = "https://rc.invalid/v1"
$env:ZEALOUS_API_KEY = $fakeKey
$env:ZEALOUS_MODEL = "rc-model"
$cfgOut = cmd /c "`"$exe`" config check 2>&1" | Out-String
Remove-Item Env:ZEALOUS_BASE_URL, Env:ZEALOUS_API_KEY, Env:ZEALOUS_MODEL
if ($cfgOut.Contains($fakeKey)) { Fail "config-check" "leaked fake key" }
Write-Host "[ok] config check no leak"

# 6) mock E2E (real_model_smoke stays ignored)
cmd /c "cargo test -p zealous-cli --test cli_e2e 2>&1" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "e2e" "cli_e2e failed" }
Write-Host "[ok] mock E2E"

# 7) zip (exe only)
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
New-Item -ItemType Directory -Path $dist -Force | Out-Null
Compress-Archive -Path $exe -DestinationPath $zipPath
Write-Host "[ok] packed: $zipName"
