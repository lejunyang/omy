# 端到端验证：缩略图按可见性加载，并预取即将进入视口的部分。
#
# 为什么要用很多文件：懒加载的意义就是「不要一次性加载全部」。
# 文件少于一屏时，懒加载与全部加载的表现完全一样，任何断言都会通过，
# 测不出东西。
#
# 数量必须远超保活范围（KEEP_MARGIN=2000px），否则**卸载逻辑永远
# 不触发**。60 张时内容总高只有 2028px，整个列表都在保活范围内，
# 「加载量受控」那条必然失败——那不是产品缺陷，是语料不够。
# 260 张约 8700px，足够让远处的图被卸载。
#
# 为什么用 CLI 而不是 GUI 来造语料：GUI 加密要走界面点选，60 个文件
# 会让脚本变得又慢又脆；而这里要验的是**渲染**行为，不是加密流程。

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
# 前端产物也要比前端源码新：tauri 编译期把 dist 内嵌进 exe。
# 这条对本次验证尤其关键——懒加载逻辑全在前端，只改前端不重新
# npm run build + cargo build 的话，跑的还是旧页面，而断言会全部
# 按旧行为失败，看上去像功能没做对。
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

$port = 9380
$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-lazy-test'
$plain = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-lazy-src'
Remove-Item $work, $plain -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work, $plain | Out-Null
$proc = $null

try {
    Write-Output "`n[1] 造 260 张真图片并加密"
    # 用 .NET 画真 PNG：随手写几个字节的假 PNG 会被解码器拒绝，
    # 那样测出来的是「素材坏了」而不是产品行为。
    # 每张画不同内容，避免内容相同导致的意外去重
    Add-Type -AssemblyName System.Drawing
    for ($i = 0; $i -lt 260; $i++) {
        $bmp = New-Object System.Drawing.Bitmap 400, 300
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $hue = ($i * 6) % 255
        $g.FillRectangle(
            (New-Object System.Drawing.SolidBrush (
                [System.Drawing.Color]::FromArgb(255, $hue, (255 - $hue), 128))),
            0, 0, 400, 300)
        $g.FillEllipse([System.Drawing.Brushes]::White, 40 + ($i % 10) * 20, 30, 180, 140)
        $g.DrawString("$i", (New-Object System.Drawing.Font 'Arial', 48),
            [System.Drawing.Brushes]::Black, 20, 200)
        $g.Dispose()
        $bmp.Save((Join-Path $plain "img$i.png"), [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
    }
    $made = (Get-ChildItem $plain -Filter *.png).Count
    Check '260 张测试图已生成' ($made -eq 260) "实际只有 $made 张"

    # 用 mobile 档 KDF：这里量的是渲染行为，KDF 强度无关，
    # 用生产参数会让 60 个文件的加密慢到没必要。
    #
    # --vault 是必须的：不指定时每个文件自成一库，260 个文件就是 260 个库，
    # GUI 解锁只会解开其中一个，界面上大部分文件仍是锁定态——那样测出来
    # 的是「共库没配」，不是懒加载行为。
    $env:OMY_LAZY_PW = 'lazy-pw'
    $pngs = Get-ChildItem $plain -Filter *.png | Sort-Object Name
    & $cli encrypt $pngs[0].FullName --output-dir $work --password-env OMY_LAZY_PW `
        --kdf-profile mobile --thumbnail auto --yes *> $null
    $rest = @($pngs | Select-Object -Skip 1 | ForEach-Object { $_.FullName })
    & $cli encrypt @rest --output-dir $work --password-env OMY_LAZY_PW `
        --kdf-profile mobile --thumbnail auto --vault $work --yes *> $null
    $enc = (Get-ChildItem $work -Filter *.omy).Count
    Check '260 个加密文件已产出' ($enc -ge 200) "实际只有 $enc 个"

    # 交叉验证：确认这些文件真的带缩略图。
    # 不验的话，若缩略图生成失败，后面的懒加载断言会因为「没有图」
    # 而全部通过，得出错误结论
    $sample = Get-ChildItem $work -Filter *.omy | Select-Object -First 5
    $withThumb = 0
    foreach ($f in $sample) {
        $j = & $cli info $f.FullName --json 2>$null | ConvertFrom-Json
        if ($j.has_thumbnail) { $withThumb++ }
    }
    Check '抽样确认文件带缩略图' ($withThumb -eq 5) "5 个样本里只有 $withThumb 个有缩略图"

    Write-Output "`n[2] 在界面里滚动，检查加载行为"
    $env:OMY_GUI_CDP_PORT = "$port"
    # 前后都要清进程：中途中断会留下占着调试端口的旧进程，
    # 之后每次验证都连到那个旧进程，跑的始终是改动之前的二进制
    taskkill /F /IM omy-gui.exe *> $null
    Start-Sleep -Milliseconds 600
    # 窗口必须 Normal 且尺寸固定：最小化时 WebView 可能不渲染，
    # 而 IntersectionObserver 依赖真实布局——窗口尺寸变了，
    # 「一屏能放多少张」也会变，断言就不稳定
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 5

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-thumb-lazy.mjs') `
        $port $work 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 懒加载探针全部通过' ($probeOut -match 'PROBE_OK') '探针输出见上'

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    if ($proc) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    taskkill /F /IM omy-gui.exe *> $null
    Remove-Item $work, $plain -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT, Env:\OMY_LAZY_PW -EA SilentlyContinue
}

if ($fail -gt 0) { exit 1 }
