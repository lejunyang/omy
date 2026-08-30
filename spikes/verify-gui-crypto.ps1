# 实测加密与解锁链路。
$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9347
$work = Join-Path $env:TEMP 'omy-crypto-test'

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

Write-Output '=== 0. 造测试素材（全部是普通文件，由 GUI 来加密）==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
Set-Content -Path (Join-Path $work '报告.txt') -Value 'quarterly report content' -Encoding UTF8
Set-Content -Path (Join-Path $work 'notes.md') -Value '# meeting notes' -Encoding UTF8
Write-Output ("  素材: " + ((Get-ChildItem $work -File | ForEach-Object { $_.Name }) -join ', '))

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 400

$env:OMY_GUI_CDP_PORT = "$port"
$err = Join-Path $env:TEMP 'omy-crypto.err'
$p = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-crypto.log') -RedirectStandardError $err
Write-Output "已启动 GUI，pid=$($p.Id)"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-gui-crypto.mjs $port $work 2>&1 | Out-String -Width 130 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}

Write-Output ''
Write-Output '--- GUI stderr ---'
if (Test-Path $err) {
    $e = Get-Content $err -Raw
    if ($e -and $e.Trim()) { Write-Output $e } else { Write-Output '  (空)' }
}
exit $code
