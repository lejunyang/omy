# 造多样的媒体素材，用于 omy-media 的独立验证。
#
# 为什么必须用真实文件：单元测试用的是手写 JSON 样本，那只能验证
# 「给定这段 JSON 能否正确解析」，无法验证「真实 ffprobe 的输出是否
# 真的长这样」。两者同源时，我对格式的误解会同时存在于样本和实现里，
# 测试全绿但真实文件失败——这正是 cat --range 那个 off-by-one 的教训。
$ErrorActionPreference = 'Continue'
$repo = 'E:\Projects\omy'
$ff   = Join-Path $repo 'tools\ffmpeg\bin\ffmpeg.exe'
$dir  = Join-Path $repo 'spikes\fixtures\media'

if (-not (Test-Path $ff)) { Write-Output "缺少 $ff"; exit 1 }
New-Item -ItemType Directory -Force -Path $dir | Out-Null

# 每项：文件名 + ffmpeg 参数 + 期望的分级（人工按文档 §5.2 判定）
$cases = @(
    @{ name = 'h264_aac.mp4';   args = @('-c:v','libx264','-preset','ultrafast','-crf','30','-c:a','aac');            desc = 'MP4 + H.264 + AAC → P1' }
    @{ name = 'h264_aac.mkv';   args = @('-c:v','libx264','-preset','ultrafast','-crf','30','-c:a','aac');            desc = 'MKV + H.264 + AAC → P2（容器不支持但可 remux）' }
    @{ name = 'vp9_opus.webm';  args = @('-c:v','libvpx-vp9','-crf','50','-b:v','0','-c:a','libopus','-cpu-used','8'); desc = 'WebM + VP9 + Opus → P1' }
    @{ name = 'h264_flac.mkv';  args = @('-c:v','libx264','-preset','ultrafast','-crf','30','-c:a','flac');           desc = 'MKV + H.264 + FLAC → FLAC 原生可解但不能进 MP4' }
    @{ name = 'mpeg4_mp3.avi';  args = @('-c:v','mpeg4','-vtag','xvid','-qscale:v','10','-c:a','libmp3lame');         desc = 'AVI + MPEG-4 ASP + MP3 → P3（编码不支持）' }
    @{ name = 'audio_only.mp3'; args = @('-vn','-c:a','libmp3lame','-b:a','128k');                                     desc = '纯音频 MP3 → P1' }
    @{ name = 'h264_only.mp4';  args = @('-an','-c:v','libx264','-preset','ultrafast','-crf','30');                    desc = '无音轨 MP4 → P1' }
)

Write-Output '=== 生成素材（8 秒，小尺寸以求快）==='
foreach ($c in $cases) {
    $out = Join-Path $dir $c.name
    if (Test-Path $out) { Remove-Item $out -Force }
    # testsrc2 有变化的画面，比纯色更能反映真实编码行为
    $base = @('-v','error','-y',
              '-f','lavfi','-i','testsrc2=size=320x180:rate=15:duration=8',
              '-f','lavfi','-i','sine=frequency=440:duration=8')
    $tail = @('-shortest', $out)
    & $ff @base @($c.args) @tail
    if ((Test-Path $out) -and (Get-Item $out).Length -gt 0) {
        Write-Output ("  OK   {0,-18} {1,9:N0} B   {2}" -f $c.name, (Get-Item $out).Length, $c.desc)
    } else {
        Write-Output ("  FAIL {0,-18} 生成失败（可能缺少该编码器）" -f $c.name)
    }
}

# 带封面的 MP3：验证 attached_pic 不被当成视频轨。
# 这是真实世界最常见的误判来源。
Write-Output ''
Write-Output '=== 生成带封面的 MP3（验证 attached_pic 处理）==='
$cover = Join-Path $dir '_cover.png'
& $ff -v error -y -f lavfi -i 'testsrc2=size=200x200:rate=1:duration=1' -frames:v 1 $cover
$mp3c = Join-Path $dir 'with_cover.mp3'
if (Test-Path $mp3c) { Remove-Item $mp3c -Force }
& $ff -v error -y -f lavfi -i 'sine=frequency=440:duration=5' -i $cover `
    -map '0:a' -map '1:v' -c:a libmp3lame -c:v copy `
    -metadata:s:v title='Album cover' -disposition:v attached_pic $mp3c
if (Test-Path $mp3c) {
    Write-Output "  OK   with_cover.mp3  $((Get-Item $mp3c).Length) B"
} else {
    Write-Output '  FAIL with_cover.mp3'
}
Remove-Item $cover -Force -ErrorAction SilentlyContinue

# faststart 版本：用于验证 moov 位置对抽帧的影响。
# 实测结论是「moov 在尾部也能抽帧，只是画质略低」，
# 这两个素材并存才能锁定该行为。
Write-Output ''
Write-Output '=== 生成 faststart 版本（对比 moov 位置的影响）==='
$src = Join-Path $dir 'h264_aac.mp4'
$fs  = Join-Path $dir 'h264_aac_faststart.mp4'
if (Test-Path $src) {
    if (Test-Path $fs) { Remove-Item $fs -Force }
    & $ff -v error -y -i $src -c copy -movflags '+faststart' $fs
    if (Test-Path $fs) {
        Write-Output "  OK   h264_aac_faststart.mp4  $((Get-Item $fs).Length) B"
    } else {
        Write-Output '  FAIL h264_aac_faststart.mp4'
    }
}

# 非媒体文件：验证 NotMedia 而非崩溃
$txt = Join-Path $dir 'not_media.txt'
Set-Content -Path $txt -Value 'This is definitely not a media file.' -Encoding UTF8
Write-Output "  OK   not_media.txt（用于验证 NotMedia 分支）"

Write-Output ''
Write-Output "素材目录：$dir"
Get-ChildItem $dir | ForEach-Object { Write-Output ("  {0,-20} {1,10:N0} B" -f $_.Name, $_.Length) }
