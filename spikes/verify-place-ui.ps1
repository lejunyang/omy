# 远程位置 UI 目检：起真实 WebDAV + GUI，驱动移动/桌面界面并截图到 spikes/shots。
# 后端断言见 verify-place-cloud.ps1；本脚本只产出供肉眼核对的截图。
#
# 跑前先构建（前端改动要先 vite build，再 cargo build -p omy-gui）：
#   cargo build -p omy-gui -p omy-cli
#   cargo build -p omy-remote --example dav_server

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\debug\omy-gui.exe'
$omy = Join-Path $root 'target\debug\omy.exe'
$dav = Join-Path $root 'target\debug\examples\dav_server.exe'
$node = 'C:\Program Files\nodejs\node.exe'
if (-not (Test-Path $node)) { $node = 'node' }

$davPort = 8799
$cdpPort = 9467
$pass = 'cloud-test-password'
$plainLen = 3 * 1024 * 1024 + 512 * 1024
$shots = Join-Path $PSScriptRoot 'shots'
New-Item -ItemType Directory -Force -Path $shots | Out-Null
Get-ChildItem $shots -Filter 'm0*-*.png' -ErrorAction SilentlyContinue | Remove-Item -Force
Get-ChildItem $shots -Filter 'd0*-*.png' -ErrorAction SilentlyContinue | Remove-Item -Force

foreach ($b in @($gui, $omy, $dav)) {
    if (-not (Test-Path $b)) { Write-Output "FAIL 缺少 $b，先按脚本头注释构建"; exit 1 }
}

$work = Join-Path $env:TEMP "omy-cloud-ui-$PID"
$davDir = Join-Path $work 'dav'
$vaultDir = Join-Path $work 'vault'
New-Item -ItemType Directory -Force -Path $davDir, $vaultDir | Out-Null
$davProc = $null
function Stop-All {
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    if ($script:davProc) { Stop-Process -Id $script:davProc.Id -Force -ErrorAction SilentlyContinue }
}

try {
    $plain = Join-Path $work 'movie.mp4'
    # 用 ffmpeg 造一段真实可播的 H.264/AAC 短视频（faststart 便于 Range 拖动），
    # 这样预览层会真正挂上 <video>；没有 ffmpeg 才退回确定性字节（无法解码）。
    $ff = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($ff) {
        & $ff.Source -y -hide_banner -loglevel error `
            -f lavfi -i 'testsrc=size=320x240:rate=24' `
            -f lavfi -i 'sine=frequency=440' `
            -t 6 -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest -movflags +faststart `
            $plain
    }
    else {
        & $node -e "require('fs').writeFileSync(process.argv[1], Buffer.alloc(+process.argv[2]).map((_,i)=>i%251))" $plain "$plainLen"
    }
    $env:OMY_CLOUD_PW = $pass
    $localOmy = Join-Path $vaultDir 'movie.mp4.omy'
    & $omy encrypt $plain -o $localOmy --password-env OMY_CLOUD_PW --kdf-profile mobile --thumbnail none 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw '加密失败' }

    Copy-Item $localOmy (Join-Path $davDir 'movie.mp4.omy')
    $cnDir = Join-Path $davDir '影视'
    New-Item -ItemType Directory -Force -Path $cnDir | Out-Null
    Copy-Item $localOmy (Join-Path $cnDir '大片 2.mp4.omy')
    Set-Content -Path (Join-Path $davDir 'plain.txt') -Value 'not encrypted' -NoNewline

    Get-Process dav_server -ErrorAction SilentlyContinue | Stop-Process -Force
    $davProc = Start-Process -FilePath $dav -ArgumentList "`"$davDir`"", "$davPort" -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 800
    Invoke-WebRequest -Uri "http://127.0.0.1:$davPort/movie.mp4.omy" -Method Head -UseBasicParsing | Out-Null

    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    $env:OMY_GUI_CDP_PORT = "$cdpPort"
    $guiErr = Join-Path $work 'gui.err'
    $guiOut = Join-Path $work 'gui.out'
    Start-Process -FilePath $gui -RedirectStandardError $guiErr -RedirectStandardOutput $guiOut | Out-Null

    $out = & $node --experimental-websocket (Join-Path $PSScriptRoot 'probe-place-ui.mjs') `
        "$cdpPort" "http://127.0.0.1:$davPort/" "$vaultDir" "$pass" "$shots" 2>&1 | Out-String
    Write-Output $out
    if ($out -notmatch 'UI_SHOTS_OK') { throw '截图探针未完成' }
    Write-Output "=== 截图已保存到 $shots ==="
    if (Test-Path $guiErr) {
        $errText = Get-Content $guiErr -Raw -ErrorAction SilentlyContinue
        if ($errText) { Write-Output "=== GUI stderr ==="; Write-Output $errText }
    }
}
finally {
    Stop-All
    Remove-Item Env:\OMY_CLOUD_PW -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
