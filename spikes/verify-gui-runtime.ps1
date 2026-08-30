# 启动真实 GUI 进程，用 CDP 验证 Vue 界面确实渲染出来。
$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$exe = Join-Path $root 'target\release\omy-gui.exe'
$port = 9344
$log = Join-Path $env:TEMP 'omy-gui-probe.log'
$err = Join-Path $env:TEMP 'omy-gui-probe.err'

if (-not (Test-Path $exe)) {
    Write-Output "FAIL 找不到 $exe"
    exit 1
}

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 400

$env:OMY_GUI_CDP_PORT = "$port"
$p = Start-Process -FilePath $exe -PassThru `
    -RedirectStandardOutput $log -RedirectStandardError $err

Write-Output "已启动 GUI，pid=$($p.Id)，CDP 端口 $port"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    # Node 20 的全局 WebSocket 要显式开启（Node 22+ 才默认可用）
    node --experimental-websocket probe-gui-vue.mjs $port 2>&1 | Out-String -Width 120 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
}

Write-Output ''
Write-Output '--- GUI 进程 stderr ---'
if (Test-Path $err) {
    $e = Get-Content $err -Raw
    if ($e -and $e.Trim()) { Write-Output $e } else { Write-Output '  (空)' }
}
exit $code
