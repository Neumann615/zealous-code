$targets = @(
    "C:\Users\31668\Desktop\work\zealous-code\scripts\package-release.ps1",
    "C:\Users\31668\Desktop\work\zealous-code\scripts\tui-acceptance-fixture.ps1"
)
$bad = 0
foreach ($t in $targets) {
    $tokens = $null
    $errs = $null
    [System.Management.Automation.Language.Parser]::ParseFile($t, [ref]$tokens, [ref]$errs)
    if ($errs.Count -gt 0) {
        $bad += 1
        Write-Host "ERR $t"
        foreach ($e in $errs) {
            Write-Host "  line $($e.Extent.StartLineNumber): $($e.Message)"
        }
    } else {
        Write-Host "OK  $t"
    }
}
if ($bad -gt 0) { exit 1 }
Write-Host "ALL OK"
