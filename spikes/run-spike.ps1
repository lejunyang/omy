# 全自动运行 Spike S1 + S5：启动带 CDP 的 spike，用 CDP 驱动测试，
# 然后检查缓存泄露。无需任何人工点击。
#
# WebView2 通过 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS 接受 Chromium 参数，
# 这是开启 CDP 的官方途径。
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
# 关键：远程调试参数必须在进程启动前设好
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port --remote-allow-origins=*"

$log = Join-Path $repo 'spikes\fixtures\spike-stdout.log'
if (Test-Path $log) { Remove-Item $log -Force }

$p = Start-Process -FilePath $exe `
    -ArgumentList @($omy, 'spike-test-password', 'video/mp4') `
    -RedirectStandardOutput $log `
    -RedirectStandardError (Join-Path $repo 'spikes\fixtures\spike-stderr.log') `
    -PassThru
Write-Output "进程 PID $($p.Id)"

Start-Sleep -Seconds 4

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
