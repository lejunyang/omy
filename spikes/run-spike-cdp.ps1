# 启动 spike 并用 CDP 驱动完成 S1 全部验证，无需人工点击。
$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$port = 9223
$exe  = Join-Path $repo 'target\release\omy-spike-webview.exe'
$enc  = Join-Path $repo 'spikes\fixtures\sample.mp4.omy'
$log  = Join-Path $repo 'spikes\fixtures\_spike-stdout.log'

# 清掉可能残留的旧进程，避免端口占用与统计混淆
Get-Process -Name 'omy-spike-webview' -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Output "结束残留进程 PID $($_.Id)"; $_.Kill(); $_.WaitForExit(3000) }
Start-Sleep -Milliseconds 500

if (-not (Test-Path $exe)) { Write-Output "缺少 $exe"; exit 1 }

Write-Output "启动 spike（CDP 端口 $port）..."
# 走 WebView2 API 注入调试端口，不用环境变量（见 main.rs 注释说明原因）
$env:OMY_SPIKE_CDP_PORT = "$port"
$p = Start-Process -FilePath $exe `
    -ArgumentList @($enc, 'spike-test-password', 'video/mp4') `
    -PassThru -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
    -WorkingDirectory $repo
Write-Output "PID $($p.Id)"

Start-Sleep -Seconds 4

# 先确认端口真的在监听，否则 CDP 驱动的失败信息会误导排查方向
$listening = $false
for ($i = 0; $i -lt 20; $i++) {
    $c = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue
    if ($c) { $listening = $true; break }
    Start-Sleep -Milliseconds 500
}
Write-Output "端口 $port 监听中: $listening"
if (-not $listening) {
    Write-Output '调试端口未打开。spike 输出：'
    if (Test-Path $log) { Get-Content $log | Select-Object -Last 30 }
    if (Test-Path "$log.err") { Get-Content "$log.err" | Select-Object -Last 20 }
}

Write-Output ''
Write-Output '=== CDP 驱动 ==='
& node (Join-Path $repo 'spikes\cdp-drive.mjs') $port
$driveExit = $LASTEXITCODE

Write-Output ''
Write-Output '=== 关闭 spike，取 Rust 端统计报告 ==='
if (-not $p.HasExited) {
    $p.CloseMainWindow() | Out-Null
    if (-not $p.WaitForExit(5000)) { $p.Kill() }
}
Start-Sleep -Milliseconds 800

if (Test-Path $log) {
    Write-Output '--- spike stdout ---'
    Get-Content $log | Select-Object -Last 60
}
if ((Test-Path "$log.err") -and (Get-Item "$log.err").Length -gt 0) {
    Write-Output '--- spike stderr ---'
    Get-Content "$log.err" | Select-Object -Last 30
}

Write-Output ''
Write-Output "CDP 驱动退出码: $driveExit"
exit $driveExit
