# 为 GUI 验证造一个测试库：多种类型的文件各加密一份。
#
# 与 spike 的 fixture 不同，这里要覆盖 GUI 的完整场景：
# 视频（能播）、图片（能显示）、文本（能读）、以及一个
# **用不同密码加密**的文件——用来验证「锁定态不泄露信息」。
param(
    [string]$OutDir = "$PSScriptRoot\fixtures\vault"
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$omy = Join-Path $repo 'target\release\omy.exe'
$media = Join-Path $repo 'spikes\fixtures\media'

if (-not (Test-Path $omy)) {
    Write-Output "缺少 $omy，请先 cargo build --release -p omy-cli"
    exit 1
}

# 素材依赖 make-media-fixtures.ps1 与 make-seek-fixtures.ps1
$srcVideo = Join-Path $media 'seek_h264_aac.mp4'
if (-not (Test-Path $srcVideo)) {
    Write-Output "缺少 $srcVideo，请先运行 make-seek-fixtures.ps1"
    exit 1
}

if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

$tmp = Join-Path $env:TEMP "omy-vault-src-$PID"
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

$PASS = 'vault-test-password'
$OTHER = 'a-completely-different-password'

Write-Output '=== 准备明文素材 ==='

# 视频：直接用 seek 素材（60 秒，60 个关键帧，适合验证 seek）
Copy-Item $srcVideo (Join-Path $tmp 'demo-video.mp4')

# 图片：造一张有确定内容的 PNG，方便验证「真的解码出来了」
$png = Join-Path $tmp 'demo-image.png'
& ffmpeg -y -v error -f lavfi -i 'testsrc2=size=480x320:duration=1:rate=1' `
    -frames:v 1 $png 2>&1 | Out-Null

# 文本：内容里放一个哨兵字符串，验证读到的确实是这个文件
$txt = Join-Path $tmp 'demo-notes.txt'
$sentinel = 'OMY-GUI-SENTINEL-7F3A'
@(
    '# omy GUI 测试文本'
    ''
    "哨兵: $sentinel"
    ''
    '这一行用于验证文本预览读到的是解密后的真实内容，'
    '而不是缓存、占位符或错误信息。'
    ''
    '多字节字符测试: 中文、日本語、Ελληνικά、Ω≈ç√'
) | Set-Content -Path $txt -Encoding UTF8

# 音频：从视频里抽一段音轨
$m4a = Join-Path $tmp 'demo-audio.m4a'
& ffmpeg -y -v error -i $srcVideo -t 10 -vn -c:a copy $m4a 2>&1 | Out-Null

Write-Output '=== 加密 ==='
$files = @(
    @{ src = 'demo-video.mp4';  thumb = $true  }
    @{ src = 'demo-image.png';  thumb = $true  }
    @{ src = 'demo-notes.txt';  thumb = $false }
    @{ src = 'demo-audio.m4a';  thumb = $false }
)

# 密码走环境变量而非命令行参数：命令行明文会进入进程列表（旁路 L12），
# CLI 因此根本没有 --password 选项
$env:OMY_TEST_PASS = $PASS
$env:OMY_TEST_OTHER = $OTHER

# 第一个文件建库，其余用 --vault 加入
$first = $true

foreach ($f in $files) {
    $in = Join-Path $tmp $f.src
    if (-not (Test-Path $in)) {
        Write-Output "  跳过（素材缺失）: $($f.src)"
        continue
    }
    $out = Join-Path $OutDir ($f.src + '.omy')
    # mobile 档位（m=32MiB t=4）是最快的：测试要跑得快，
    # 而 KDF 强度不是这里要验证的东西
    $cliArgs = @(
        'encrypt', $in, '-o', $out,
        '--password-env', 'OMY_TEST_PASS'
    )
    # 第一个文件建库，之后的都用 --vault 加入同一个库。
    # 不这么做的话每次调用都会生成新的随机 salt，
    # 5 个文件就是 5 个互不相通的库——同一密码也要派生 5 次
    if ($first) {
        $cliArgs += @('--kdf-profile', 'mobile')
        $first = $false
    } else {
        $cliArgs += @('--vault', $OutDir)
    }
    if (-not $f.thumb) { $cliArgs += @('--thumbnail', 'none') }
    & $omy @cliArgs 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Output "  加密失败: $($f.src)"
        exit 1
    }
    $size = (Get-Item $out).Length
    Write-Output ("  {0,-20} → {1,10:N0} B" -f $f.src, $size)
}

# 关键：一个用**其他密码**加密的文件。
# GUI 必须把它显示为锁定态，且不泄露文件名、大小、缩略图。
$secretSrc = Join-Path $tmp 'MUST-NOT-APPEAR.txt'
'如果这段文字出现在界面上，说明锁定态泄露了信息' |
    Set-Content -Path $secretSrc -Encoding UTF8
$secretOut = Join-Path $OutDir 'other-password.omy'
& $omy encrypt $secretSrc -o $secretOut `
    --password-env OMY_TEST_OTHER --kdf-profile mobile 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Output '  加密失败: other-password'
    exit 1
}
Write-Output ("  {0,-20} → 用另一个密码加密（验证锁定态）" -f 'other-password')

Remove-Item Env:\OMY_TEST_PASS -ErrorAction SilentlyContinue
Remove-Item Env:\OMY_TEST_OTHER -ErrorAction SilentlyContinue
Remove-Item $tmp -Recurse -Force

Write-Output ''
Write-Output "测试库: $OutDir"
Get-ChildItem $OutDir | ForEach-Object {
    Write-Output ("  {0,-28} {1,10:N0} B" -f $_.Name, $_.Length)
}
Write-Output ''
Write-Output "密码: $PASS"
Write-Output "哨兵: $sentinel"
