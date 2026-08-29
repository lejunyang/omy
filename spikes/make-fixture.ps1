# 生成 Spike S1/S5 用的真实测试视频，并用 omy 加密。
#
# 关键点：
# - 必须 -movflags +faststart：moov 原子前置，<video> 才能只读开头就拿到
#   时长并立即 seek。若 moov 在尾部，播放器要先跳到文件末尾读元数据，
#   这本身就是一次 Range 请求，会混淆 S1 的判定。
# - 时长要足够长（60s）且有明显的视觉时间标记，便于肉眼确认 seek 真的生效，
#   而不是画面没变却报告成功。
# - 分辨率与码率刻意选小，让文件在几 MB 量级，便于快速迭代。
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$ff   = Join-Path $repo 'tools\ffmpeg\bin\ffmpeg.exe'
$out  = Join-Path $repo 'spikes\fixtures'
New-Item -ItemType Directory -Path $out -Force | Out-Null

if (-not (Test-Path $ff)) { Write-Output "缺少 $ff"; exit 1 }

$mp4 = Join-Path $out 'sample.mp4'
Write-Output '=== 生成测试视频（60s，含时间码水印）==='

# testsrc2 自带变化的画面；drawtext 叠加秒数，肉眼可直接验证 seek 是否真的跳转
$vf = "drawtext=text='%{pts\:hms}':fontsize=48:fontcolor=white:x=(w-text_w)/2:y=h-80:box=1:boxcolor=black@0.5"
& $ff -y -hide_banner -loglevel error `
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=60" `
    -f lavfi -i "sine=frequency=440:duration=60" `
    -vf $vf `
    -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p -g 30 `
    -c:a aac -b:a 64k `
    -movflags +faststart `
    $mp4
if ($LASTEXITCODE -ne 0) { Write-Output 'ffmpeg 失败'; exit 1 }

$len = (Get-Item $mp4).Length
Write-Output ("生成: {0}（{1:N0} 字节）" -f $mp4, $len)

# 验证 faststart 真的生效：moov 应出现在 mdat 之前
$head = [System.IO.File]::ReadAllBytes($mp4)[0..2047]
$txt  = [System.Text.Encoding]::ASCII.GetString($head)
$moov = $txt.IndexOf('moov')
$mdat = $txt.IndexOf('mdat')
if ($moov -ge 0 -and ($mdat -lt 0 -or $moov -lt $mdat)) {
    Write-Output "faststart 确认：moov 在前 2 KiB 内（偏移 $moov）"
} else {
    Write-Output "警告：前 2 KiB 未见 moov，faststart 可能未生效"
}

# 用 omy 加密
$omy = Join-Path $repo 'target\release\omy.exe'
if (-not (Test-Path $omy)) {
    Write-Output "缺少 $omy，请先 cargo build --release -p omy-cli"
    exit 1
}
$pwFile = Join-Path $out 'pw.txt'
Set-Content -Path $pwFile -Value 'spike-test-password' -NoNewline

$enc = "$mp4.omy"
if (Test-Path $enc) { Remove-Item $enc -Force }

Write-Output ''
Write-Output '=== 用 omy 加密（mobile 档位以加快 spike 迭代）==='
# 刻意用较小的 chunk_size：让一次 seek 只需解密少量数据，
# 便于观察「按需解密」是否真的生效
& $omy encrypt --password-file $pwFile --kdf-profile mobile --chunk-size 256K $mp4
if ($LASTEXITCODE -ne 0) { Write-Output 'omy encrypt 失败'; exit 1 }

& $omy info $enc

Write-Output ''
Write-Output '=== 就绪 ==='
Write-Output "明文: $mp4"
Write-Output "密文: $enc"
Write-Output "密码: spike-test-password"
