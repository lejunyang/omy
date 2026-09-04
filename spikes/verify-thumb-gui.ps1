# 端到端验证：GUI 里真的能看到缩略图。
#
# 为什么必须验到「用户看到的那一层」：本轮修的三个缺陷都在这条链上，
# 而它们都能骗过较浅的检查——
#
#   1. --thumbnail auto 一律走视频抽帧，图片永远拿不到缩略图。
#      读取侧代码（has_thumbnail、/thumb 路由、EntryCard 的 img）一直都在，
#      只是永远读到空，看起来像「功能有，只是这个文件没有图」。
#   2. GUI 从不调用 prepare，thumbnail 字段恒为空。
#   3. 缩略图实际是 WebP，而 /thumb 硬编码 Content-Type: image/jpeg。
#      本地 WebView 会自己嗅探真实格式照样显示，所以截图、看 DOM 都正常。
#
# 探针按六层分别断言，失败时能直接指出断在哪一层，不必再手工二分。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
$gui = Join-Path $root 'target\release\omy-gui.exe'

if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }
if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 新鲜度守卫：exe 必须比源码新，否则跑的是改动之前的二进制
$binTime = (Get-Item $gui).LastWriteTime
foreach ($c in 'omy-gui', 'omy-core', 'omy-media') {
    $newer = Get-ChildItem (Join-Path $root "crates\$c\src") -Recurse -Filter *.rs |
        Where-Object { $_.LastWriteTime -gt $binTime }
    if ($newer) {
        Write-Output "FAIL omy-gui.exe 比 $c 源码旧（$($newer[0].Name)），先 cargo build --release -p omy-gui"
        exit 1
    }
}
# 前端产物也要比前端源码新：tauri 编译期把 dist 内嵌进 exe，
# 只改前端不重跑 npm run build 的话，exe 里还是旧页面
$dist = Join-Path $root 'crates\omy-gui\frontend\dist\index.html'
if (Test-Path $dist) {
    $distTime = (Get-Item $dist).LastWriteTime
    $newerFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse |
        Where-Object { -not $_.PSIsContainer -and $_.LastWriteTime -gt $distTime }
    if ($newerFe) {
        Write-Output "FAIL 前端产物比源码旧（$($newerFe[0].Name)），先 npm run build"
        exit 1
    }
    if ($distTime -gt $binTime) {
        Write-Output 'FAIL exe 比前端产物旧，先 cargo build --release -p omy-gui'
        exit 1
    }
}

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

$port = 9379
$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-thumb-gui-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$proc = $null

try {
    Write-Output "`n[1] 造一张真图片"
    # 用 .NET 画一张真 PNG：随手写几个字节的假 PNG 会被图片解码器拒绝，
    # 那样测出来的是「素材坏了」而不是产品行为
    Add-Type -AssemblyName System.Drawing
    $bmp = New-Object System.Drawing.Bitmap 640, 480
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.FillRectangle([System.Drawing.Brushes]::CornflowerBlue, 0, 0, 640, 480)
    $g.FillEllipse([System.Drawing.Brushes]::Orange, 120, 80, 400, 320)
    $g.Dispose()
    $bmp.Save((Join-Path $work 'shot.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $sz = (Get-Item (Join-Path $work 'shot.png')).Length
    Check '测试图片已生成' ($sz -gt 500) "只有 $sz 字节"

    Write-Output "`n[2] 通过 GUI 界面加密并查看缩略图"
    # 变量名必须是 OMY_GUI_CDP_PORT，写错的话 GUI 起来但不开调试端口
    $env:OMY_GUI_CDP_PORT = "$port"
    # 前后都要清进程：中途中断会留下占着调试端口的旧进程，
    # 之后每次验证都连到那个旧进程，跑的始终是改动之前的二进制
    taskkill /F /IM omy-gui.exe *> $null
    Start-Sleep -Milliseconds 600
    # 窗口用 Normal：最小化时 WebView 可能不渲染，img 的 naturalWidth
    # 会是 0，把正常的缩略图判成失败
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 5

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-thumb-gui.mjs') `
        $port $work 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 探针全部通过' ($probeOut -match 'PROBE_OK') '探针输出见上'

    Write-Output "`n[3] 交叉验证：CLI 也认为该文件带缩略图"
    # 不只信 GUI 自己的报告：GUI 读的是自己的状态缓存，
    # 缓存对了但文件里没写进去也会显示成功
    $enc = Get-ChildItem $work -Filter *.omy | Select-Object -First 1
    if ($enc) {
        $j = & $cli info $enc.FullName --json 2>$null | ConvertFrom-Json
        Check 'CLI 报告 has_thumbnail=true' ($j.has_thumbnail -eq $true) `
            "info 实际返回 has_thumbnail=$($j.has_thumbnail)"
    } else {
        Check 'GUI 产出了 .omy' $false '目录里没有 .omy'
    }

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    if ($proc) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    taskkill /F /IM omy-gui.exe *> $null
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
}

if ($fail -gt 0) { exit 1 }
