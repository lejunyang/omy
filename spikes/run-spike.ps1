# 全自动运行 Spike S1 + S5：启动带 CDP 的 spike，用 CDP 驱动测试，
# 然后检查缓存泄露。无需任何人工点击。
#
# 调试端口的注入方式（已实测确认）：
# 通过 OMY_SPIKE_CDP_PORT 让 spike 自己在代码里调
# WebviewWindowBuilder::additional_browser_args 注入 --remote-debugging-port。
#
# 为什么不用 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS 环境变量：
#   1. wry 0.55.1 在 webview2/mod.rs 里用 unwrap_or_else 显式调用
#      options.set_additional_browser_arguments(...)，WebView2 API 传参
#      会覆盖环境变量，因此设了也不生效；
#   2. WebView2 Runtime >= 150 在宿主进程 elevated 时会直接丢弃该环境变量
#      （tauri-apps/wry#1782），只有 HKLM 策略与 API 传参被尊重。
# 另外 TAURI_REMOTE_DEBUGGING_PORT 在 tauri 2.11.5 中并不存在（已查源码）。
param(
    [int]$Port = 9333,
    [switch]$KeepOpen
)
$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$exe   = Join-Path $repo 'target\release\omy-spike-webview.exe'
$omy   = Join-Path $repo 'spikes\fixtures\sample.mp4.omy'
$drive = Join-Path $repo 'spikes\cdp-drive.mjs'

foreach ($f in @($exe, $omy, $drive)) {
    if (-not (Test-Path $f)) { Write-Output "缺少 $f"; exit 1 }
}

# 清理可能残留的旧实例，避免端口冲突与统计串扰
Get-Process -Name 'omy-spike-webview' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

Write-Output '=== S5 阶段一：记录缓存基线 ==='
& pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $repo 'spikes\check-cache-leak.ps1') -Phase baseline

Write-Output ''
Write-Output "=== 启动 spike（CDP 端口 $Port）==="
$env:OMY_SPIKE_CDP_PORT = "$Port"

$log = Join-Path $repo 'spikes\fixtures\spike-stdout.log'
if (Test-Path $log) { Remove-Item $log -Force }

$p = Start-Process -FilePath $exe `
    -ArgumentList @($omy, 'spike-test-password', 'video/mp4') `
    -RedirectStandardOutput $log `
    -RedirectStandardError (Join-Path $repo 'spikes\fixtures\spike-stderr.log') `
    -PassThru
Write-Output "进程 PID $($p.Id)"

# 等端口真正进入监听，而不是盲等固定秒数：
# 端口没开时 CDP 驱动的报错会把排查方向带偏
$listening = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Milliseconds 500
    if (Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue) {
        $listening = $true; break
    }
}
Write-Output "调试端口 $Port 监听中: $listening"
if (-not $listening) {
    Write-Output '调试端口未打开，spike 输出：'
    if (Test-Path $log) { Get-Content $log | Select-Object -Last 30 }
    if (-not $p.HasExited) { $p | Stop-Process -Force }
    exit 1
}

Write-Output ''
Write-Output '=== S1：用 CDP 驱动测试 ==='
& node $drive $Port
$s1 = $LASTEXITCODE

Write-Output ''
Write-Output '=== spike 进程的 stdout（Rust 端的真实解密日志）==='
if (Test-Path $log) {
    Get-Content $log | Select-Object -First 40
}

if (-not $KeepOpen) {
    Write-Output ''
    Write-Output '=== 关闭 spike 以取得统计报告 ==='
    # 先尝试优雅关闭，让 Rust 的 print_report 有机会执行
    $p.CloseMainWindow() | Out-Null
    Start-Sleep -Seconds 3
    if (-not $p.HasExited) { $p | Stop-Process -Force }
    Start-Sleep -Seconds 1
    if (Test-Path $log) {
        $all = Get-Content $log -Raw
        $idx = $all.IndexOf('Spike S1 结果')
        if ($idx -ge 0) {
            Write-Output ''
            Write-Output '--- Rust 端统计报告 ---'
            Write-Output $all.Substring($idx - 70)
        }
    }
}

Write-Output ''
Write-Output '=== S5 阶段二：扫描缓存是否落盘明文 ==='
& pwsh -NoProfile -ExecutionPolicy Bypass -File (Join-Path $repo 'spikes\check-cache-leak.ps1')
$s5 = $LASTEXITCODE

Write-Output ''
Write-Output ('#' * 66)
Write-Output ("S1: {0}    S5: {1}" -f
    $(if ($s1 -eq 0) { '通过' } else { '未通过' }),
    $(if ($s5 -eq 0) { '通过' } else { '未通过' }))
Write-Output ('#' * 66)

if ($s1 -ne 0 -or $s5 -ne 0) { exit 1 }
