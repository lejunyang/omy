# 生成用于 seek 验证的素材：足够多的关键帧与 Cluster。
#
# 原有的 h264_aac.mkv 是 8 秒、单关键帧、2 个 Cluster——
# 它能验证"整文件 remux"，但验证不了"从中间某点开始 remux"，
# 而后者正是 P2 seek 的核心。
#
# 这里造 60 秒、每秒一个关键帧（-g 15 且 15fps）的素材，
# 并强制较小的 cluster 时长，让 Cluster 数量足够多。

$ErrorActionPreference = 'Stop'
$out = 'E:\Projects\omy\spikes\fixtures\media'
New-Item -ItemType Directory -Path $out -Force | Out-Null

function Report {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        Write-Output "  ❌ 未生成: $([System.IO.Path]::GetFileName($Path))"
        return
    }
    $sz = (Get-Item $Path).Length
    $n = [System.IO.Path]::GetFileName($Path)
    # 数关键帧
    $pk = cmd /c "ffprobe -hide_banner -v quiet -print_format json -show_packets -select_streams v:0 `"$Path`" 2>nul"
    $kf = 0
    try {
        $o = $pk | ConvertFrom-Json
        $kf = ($o.packets | Where-Object { $_.flags -like 'K*' }).Count
    } catch { }
    # 数 Cluster（仅 mkv）
    $cl = 0
    if ($n -like '*.mkv') {
        $b = [System.IO.File]::ReadAllBytes($Path)
        for ($i = 0; $i -lt $b.Length - 4; $i++) {
            if ($b[$i] -eq 0x1F -and $b[$i+1] -eq 0x43 -and $b[$i+2] -eq 0xB6 -and $b[$i+3] -eq 0x75) { $cl++ }
        }
    }
    Write-Output "  ✅ $n  $sz 字节  关键帧 $kf 个  Cluster $cl 个"
}

Write-Output '=== 生成 seek 验证素材 ==='

# 60 秒，15fps，每 15 帧一个关键帧 → 60 个关键帧
# -cluster_time_limit 让 Matroska muxer 切更多 cluster
$seekMkv = Join-Path $out 'seek_h264_aac.mkv'
Write-Output '生成 seek_h264_aac.mkv（60s，每秒一个关键帧）...'
cmd /c "ffmpeg -hide_banner -v error -y -f lavfi -i testsrc2=size=320x180:rate=15:duration=60 -f lavfi -i sine=frequency=440:duration=60 -c:v libx264 -preset ultrafast -g 15 -keyint_min 15 -sc_threshold 0 -pix_fmt yuv420p -c:a aac -b:a 64k -cluster_time_limit 2000 `"$seekMkv`" 2>nul"
Report $seekMkv

# 同内容的 MP4，用于对照（faststart，moov 在前）
$seekMp4 = Join-Path $out 'seek_h264_aac.mp4'
Write-Output '生成 seek_h264_aac.mp4（同内容对照）...'
cmd /c "ffmpeg -hide_banner -v error -y -i `"$seekMkv`" -c copy -movflags +faststart `"$seekMp4`" 2>nul"
Report $seekMp4

Write-Output ''
Write-Output '完成。'
