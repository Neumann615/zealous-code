# TUI 手工验收夹具(spec 90 §5 / 110 收口)
# 用法: powershell -File scripts\tui-acceptance-fixture.ps1 [-Zealous <exe 路径>]
# 产出: %TEMP%\zealous-tui-acceptance\ 下两个仓库
#   main\    —— 正常任务验收用(README + 大文件 edit 场景)
#   corrupt\ —— 损坏账本场景用(events.jsonl 中间损坏 + torn tail 两份)
param(
    [string]$Zealous = "$PSScriptRoot\..\target\debug\zealous.exe"
)
$ErrorActionPreference = "Stop"

$exe = Resolve-Path $Zealous -ErrorAction Stop
$base = Join-Path $env:TEMP "zealous-tui-acceptance"
if (Test-Path $base) { Remove-Item -Recurse -Force $base }
New-Item -ItemType Directory -Path $base | Out-Null

function New-Repo {
    param([string]$Dir)
    New-Item -ItemType Directory -Path $Dir | Out-Null
    git -C $Dir init -q
    git -C $Dir -c user.name=t -c user.email=t@t add -A 2>$null
}

# ---- main 仓库:正常任务 + 大文件 edit 审批场景 ----
$main = Join-Path $base "main"
New-Repo $main
Set-Content -Path (Join-Path $main "README.md") -Value "tui acceptance fixture`nsecond line`n"
# 大文件:~200KB 文本,edit_file 审批摘要将超过一屏
$big = ("lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor " * 80) + "`n"
Set-Content -Path (Join-Path $main "big.txt") -Value $big
git -C $main add -A
git -C $main -c user.name=t -c user.email=t@t commit -q -m init
# brief 文件(任务表单引用)
Set-Content -Path (Join-Path $main "BRIEF.md") -Value "把 big.txt 的第一段替换为 REPLACED-BY-MODEL,保持其余内容不变。完成后汇报修改前后文件大小。"
Write-Host "[main] $main"

# ---- corrupt 仓库:损坏账本 + torn tail 两份任务 ----
$corrupt = Join-Path $base "corrupt"
New-Repo $corrupt
Set-Content -Path (Join-Path $corrupt "README.md") -Value "corrupt fixture`n"
git -C $corrupt add -A
git -C $corrupt -c user.name=t -c user.email=t@t commit -q -m init
# 用 CLI 真实创建两个任务(走真实账本写入)
& $exe task new "corrupt: middle broken" | Out-Null
& $exe task new "corrupt: torn tail" | Out-Null
$tasks = Get-ChildItem (Join-Path $corrupt ".zealous\tasks") -Directory
$ledgers = Get-ChildItem (Join-Path $corrupt ".zealous\tasks") -Recurse -Filter events.jsonl
# 任务 1:中间损坏(第 1 行改非法 JSON)
$p1 = $ledgers[0].FullName
$lines = [System.IO.File]::ReadAllLines($p1)
$lines[0] = "{{{ corrupted"
[System.IO.File]::WriteAllLines($p1, $lines)
# 任务 2:残缺尾行
$p2 = $ledgers[1].FullName
[System.IO.File]::AppendAllText($p2, '{"seq":99,"ts":"2026')
Write-Host "[corrupt] $corrupt"
Write-Host "  tasks: $($tasks.Count) 个账本已就位(1 中间损坏 + 1 torn tail)"

Write-Host ""
Write-Host "验收步骤:"
Write-Host "  1. cd $main"
Write-Host "  2. $exe tui"
Write-Host "  3. 按 docs/acceptance/tui-alpha.md 逐项执行并记录"
Write-Host "  4. 损坏账本场景:cd $corrupt 后再次运行 tui"
