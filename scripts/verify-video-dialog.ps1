# 视频处理对话框的端到端验证。
#
# # 为什么必须这么验证
#
# 编译通过、单测通过都不代表 WebView 里真的放得出画面。这个对话框的核心
# 是「进度条 / 时间码 / 画面是同一个值」，而三者不同步时界面上完全看不出来
# ——用户拖到某一帧，提交的却是别的时间点，要到看见缩略图才发现。
#
# # 必须跑两遍
#
# 「压缩可不可用」由 FFmpeg 能力决定，两半分支要分别覆盖：
#
#   pwsh -File scripts\verify-video-dialog.ps1
#   pwsh -File scripts\verify-video-dialog.ps1 -BuiltinFfmpeg <内置产物路径>
#
# 只跑装了完整版的那次，「没有编码器时压缩要禁用」这半边永远不会执行到，
# 把它写成永远可用也能全绿。
param(
    [int]$Port = 9466,
    [string]$BuiltinFfmpeg = ''
)
$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$exe = Join-Path $repo 'target\release\omy-gui.exe'
$probe = Join-Path $repo 'spikes\probe-video-dialog.mjs'
$media = Join-Path $repo 'spikes\fixtures\media'

foreach ($f in @($exe, $probe)) {
    if (-not (Test-Path $f)) { Write-Output "缺少 $f"; exit 1 }
}

# 源码比二进制新时拒绝运行。cargo 比较 mtime 后可能复用旧二进制，
# 于是「改了代码却毫无效果」——这个假象极难识破，因为编译确实跑了
$newest = Get-ChildItem (Join-Path $repo 'crates\omy-gui') -Recurse -File `
        -Include '*.rs', '*.js', '*.css', '*.vue', '*.html', '*.json' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch 'node_modules|[\\/]dist[\\/]' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newest -and $newest.LastWriteTime -gt (Get-Item $exe).LastWriteTime) {
    Write-Output "二进制比源码旧（$($newest.Name) 更新于 $($newest.LastWriteTime)）"
    Write-Output '请先运行: cargo build --release -p omy-gui'
    exit 1
}

# 跑之前清进程。中途中断会留下一个仍占着调试端口的旧进程，之后每次验证
# 都会连到它，跑的始终是改动之前的二进制。唯一的线索是去查端口占用
taskkill /F /IM omy-gui.exe 2>$null | Out-Null
Start-Sleep -Milliseconds 500

if (-not (Test-Path $media)) {
    Write-Output "缺少测试素材目录 $media"
    exit 1
}
$sample = Join-Path $media 'probe-video.mp4'
if (-not (Test-Path $sample)) {
    $src = Join-Path $repo 'spikes\fixtures\sample.mp4'
    if (Test-Path $src) { Copy-Item $src $sample -Force }
    else { Write-Output "缺少测试视频，且找不到 $src"; exit 1 }
}

# 再造一个**带 ASS 字幕**的素材。没有它的话字幕那几条断言只会走到
# 「无字幕」分支，而 ASS 的取舍选项是这次改动的重点，等于没测到。
# 用系统 ffmpeg 造（需要字幕编码器），造不出来就跳过而不是失败
$assSample = Join-Path $media 'probe-ass.mkv'
if (-not (Test-Path $assSample)) {
    $sysFf = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
    if ($sysFf) {
        $srt = Join-Path $env:TEMP 'omy-probe-sub.srt'
        @"
1
00:00:00,000 --> 00:00:02,000
probe subtitle line

2
00:00:02,000 --> 00:00:04,000
second line
"@ | Set-Content -Path $srt -Encoding UTF8
        & $sysFf -hide_banner -loglevel error -y `
            -f lavfi -i "testsrc=size=320x240:rate=15:duration=4" `
            -f lavfi -i "sine=frequency=440:duration=4" -i $srt `
            -map 0:v -map 1:a -map 2:s -c:v libx264 -c:a aac -c:s ass `
            -t 4 -f matroska $assSample 2>&1 | Out-Null
        if (Test-Path $assSample) { Write-Output '已生成带 ASS 字幕的素材' }
    } else {
        Write-Output '注意：无系统 ffmpeg，跳过生成带字幕素材，ASS 分支测不到'
    }
}

$env:OMY_GUI_CDP_PORT = "$Port"
# 原生目录选择器是 OS 窗口，CDP 点不到。设了这个变量后 pick_folder
# 直接返回该路径而不弹窗，让自动化能跑完整链路
$env:OMY_GUI_PICK_FOLDER = $media

# 用脚本参数而不是读环境变量：`& script.ps1` 与 `-File` 的进程边界不同，
# 实测外层设的 $env: 传不进来，表现是「设了却没生效」且毫无提示——
# 探针照样全绿，只是测的是另一半分支
if ($BuiltinFfmpeg) {
    if (-not (Test-Path $BuiltinFfmpeg)) { Write-Output "找不到 $BuiltinFfmpeg"; exit 1 }
    $env:OMY_FFMPEG = $BuiltinFfmpeg
    $env:OMY_FFPROBE = $BuiltinFfmpeg -replace 'ffmpeg\.exe$', 'ffprobe.exe'
    Write-Output "FFmpeg: 内置裁剪版 $BuiltinFfmpeg"
} else {
    Remove-Item Env:\OMY_FFMPEG -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_FFPROBE -ErrorAction SilentlyContinue
    Write-Output 'FFmpeg: 系统默认'
}

$log = Join-Path $repo 'spikes\fixtures\vprobe-stdout.log'
$errlog = Join-Path $repo 'spikes\fixtures\vprobe-stderr.log'
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
}
if (-not $listening) {
    Write-Output "CDP 端口 $Port 未监听"
    Get-Content $errlog -ErrorAction SilentlyContinue | Select-Object -Last 20
    taskkill /F /IM omy-gui.exe 2>$null | Out-Null
    exit 1
}

# node 由 osdk 管理，shim 在嵌套进程里可能失效，所以解析出真实路径再调
$nodeExe = 'node'
$nodeDir = (& osdk -q where node 2>&1 | Out-String).Trim() -split "`r?`n" | Select-Object -First 1
if ($nodeDir -and (Test-Path (Join-Path $nodeDir 'node.exe'))) {
    $nodeExe = Join-Path $nodeDir 'node.exe'
}

& $nodeExe --experimental-websocket $probe $Port
$code = $LASTEXITCODE

if ($code -ne 0) {
    Write-Output ''
    Write-Output '=== GUI stderr（最后 20 行）==='
    Get-Content $errlog -ErrorAction SilentlyContinue | Select-Object -Last 20
}

# 跑完也要清，理由同开头
taskkill /F /IM omy-gui.exe 2>$null | Out-Null
exit $code
