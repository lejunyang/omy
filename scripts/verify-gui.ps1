# 全自动跑 GUI 端到端验证：启动带 CDP 的 GUI，用 CDP 驱动，无需人工点击。
#
# 为什么必须这么验证：编译通过、单测通过都不代表 WebView 里真的
# 放得出画面。协议返回 200 也不等于视频能播——spike 阶段就踩过
# 「协议正常但 <video> 报 code=4」的坑（原因是密文偏移错了 656 字节）。
param(
    [int]$Port = 9444,
    [switch]$KeepOpen
)
$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$exe = Join-Path $repo 'target\release\omy-gui.exe'
$vault = Join-Path $repo 'spikes\fixtures\vault'
$drive = Join-Path $repo 'spikes\cdp-gui.mjs'

foreach ($f in @($exe, $drive)) {
    if (-not (Test-Path $f)) { Write-Output "缺少 $f"; exit 1 }
}
if (-not (Test-Path $vault)) {
    Write-Output "缺少测试库 $vault，请先运行 spikes\make-gui-vault.ps1"
    exit 1
}

# 源码比二进制新时拒绝运行（缺陷 #11 的教训：
# 用陈旧二进制跑出的失败会把排查方向带偏几十分钟）
$newestSrc = Get-ChildItem (Join-Path $repo 'crates\omy-gui') -Recurse -File -Include '*.rs', '*.js', '*.css', '*.html', '*.json' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc -and $newestSrc.LastWriteTime -gt (Get-Item $exe).LastWriteTime) {
    Write-Output "二进制比源码旧（$($newestSrc.Name) 更新于 $($newestSrc.LastWriteTime)）"
    Write-Output '请先运行: cargo build --release -p omy-gui'
    exit 1
}

Get-Process -Name 'omy-gui' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

Write-Output "=== 启动 GUI（CDP 端口 $Port）==="
$env:OMY_GUI_CDP_PORT = "$Port"

$log = Join-Path $repo 'spikes\fixtures\gui-stdout.log'
$errlog = Join-Path $repo 'spikes\fixtures\gui-stderr.log'
foreach ($f in @($log, $errlog)) { if (Test-Path $f) { Remove-Item $f -Force } }

$p = Start-Process -FilePath $exe -RedirectStandardOutput $log `
    -RedirectStandardError $errlog -PassThru
Write-Output "进程 PID $($p.Id)"

# 等端口真正监听，而不是盲等固定秒数
$listening = $false
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    if (Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue) {
        $listening = $true; break
    }
    if ($p.HasExited) {
        Write-Output "进程已退出，退出码 $($p.ExitCode)"
        if (Test-Path $errlog) { Get-Content $errlog | Select-Object -Last 20 }
        exit 1
    }
}
Write-Output "调试端口 $Port 监听中: $listening"
if (-not $listening) {
    Write-Output 'GUI 输出：'
    if (Test-Path $errlog) { Get-Content $errlog | Select-Object -Last 30 }
    if (-not $p.HasExited) { $p | Stop-Process -Force }
    exit 1
}

Write-Output ''
& node $drive $Port $vault
$code = $LASTEXITCODE

if (Test-Path $errlog) {
    $e = Get-Content $errlog -ErrorAction SilentlyContinue
    if ($e) {
        Write-Output ''
        Write-Output '--- GUI stderr ---'
        $e | Select-Object -First 20
    }
}

if (-not $KeepOpen) {
    $p.CloseMainWindow() | Out-Null
    Start-Sleep -Seconds 2
    if (-not $p.HasExited) { $p | Stop-Process -Force }
}

exit $code
