# Spike：验证文档 §6.2 设想的 seek 方案是否可行。
#
# 背景：Q4 已证实**输入 seek（-ss 在 -i 前）在管道下失效**
#   → "Seek to desired resync point failed"，产物只有 ftyp+moov 无 moof。
# 输出 seek（-ss 在 -i 后）可用，但要从头解完整个流才能到达目标时间点，
# 对 2 小时的电影 seek 到 1:50:00 意味着解 110 分钟的数据——不可接受。
#
# 文档 §6.2 的方案是绕开这个限制：
#   查 MKV Cues 索引 → 算出覆盖 t 秒的 cluster 字节区间
#   → 只解密并只喂这段字节 → FFmpeg demux → fMP4
#
# 本 spike 要回答：
#   Q7 从文件中间某个 cluster 边界截取的 MKV 片段，FFmpeg 能否 demux？
#   Q8 需要带上文件头（EBML header + Tracks）吗？
#   Q9 头 + 中间 cluster 拼接后能否 remux 成可播放的 fMP4？
#   Q10 能否从 MKV 中读出 Cues 索引（cluster 的时间→字节映射）？
#
# 若 Q9 成立，P2 的 seek 就能复用与 P1 完全相同的「解密字节区间」逻辑，
# 这正是文档所说的「底层逻辑与 P1 完全共用」。

$ErrorActionPreference = 'Continue'
$repo = 'E:\Projects\omy'
$fix  = Join-Path $repo 'spikes\fixtures\media'
$work = Join-Path $env:TEMP ('omy_seek_spike_' + [guid]::NewGuid().ToString('N').Substring(0,8))
New-Item -ItemType Directory -Path $work -Force | Out-Null

$mkv = Join-Path $fix 'seek_h264_aac.mkv'
if (-not (Test-Path $mkv)) {
    Write-Output "找不到 seek 素材，请先运行 spikes/make-seek-fixtures.ps1"
    exit 2
}
$bytes = [System.IO.File]::ReadAllBytes($mkv)
Write-Output '=== Spike：MKV 区间截取 + remux（P2 seek 方案）==='
Write-Output "素材: seek_h264_aac.mkv  $($bytes.Length) 字节（60s，60 关键帧，60 Cluster）"
Write-Output ''

# ---------- Q10：读出 Cues / cluster 位置 ----------
Write-Output '--- Q10：能否拿到 cluster 的时间→字节映射 ---'
# ffprobe 能列出每个 packet 的字节位置，这是最直接的验证方式
$pkts = cmd /c "ffprobe -hide_banner -v quiet -print_format json -show_packets -select_streams v:0 `"$mkv`" 2>nul"
try {
    $pj = $pkts | ConvertFrom-Json
    $keyPkts = $pj.packets | Where-Object { $_.flags -like 'K*' }
    Write-Output "  视频 packet 总数: $($pj.packets.Count)，关键帧 $($keyPkts.Count) 个"
    Write-Output "  前 5 个关键帧的 (时间, 文件偏移):"
    foreach ($k in ($keyPkts | Select-Object -First 5)) {
        Write-Output "    t=$($k.pts_time)s  pos=$($k.pos)  size=$($k.size)"
    }
    $script:keyframes = $keyPkts
} catch {
    Write-Output "  无法解析 packet 列表"
    $script:keyframes = @()
}
Write-Output ''

# ---------- 找 Cluster 起始位置 ----------
# Matroska Cluster 的 EBML ID 是 0x1F43B675。
# 注意：裸扫字节序列理论上可能在媒体数据里误命中，
# 但作为 spike 足够——真实实现会走正规的 EBML 解析。
Write-Output '--- 定位 Cluster 边界（EBML ID 1F 43 B6 75）---'
$clusterOffsets = New-Object System.Collections.Generic.List[int]
$limit = $bytes.Length - 4
$i = 0
while ($i -lt $limit) {
    if ($bytes[$i] -eq 0x1F) {
        if ($bytes[$i+1] -eq 0x43 -and $bytes[$i+2] -eq 0xB6 -and $bytes[$i+3] -eq 0x75) {
            [void]$clusterOffsets.Add($i)
            $i += 4
            continue
        }
    }
    $i++
}
Write-Output "  找到 $($clusterOffsets.Count) 个 Cluster"
if ($clusterOffsets.Count -gt 0) {
    $show = $clusterOffsets | Select-Object -First 6
    Write-Output "  前几个偏移: $($show -join ', ')"
    $firstCluster = $clusterOffsets[0]
    Write-Output "  首个 Cluster 在 $firstCluster → 之前的 $firstCluster 字节是头部（EBML+Segment info+Tracks）"
}
Write-Output ''

function Test-Remux {
    param([string]$Label, [byte[]]$Data, [string]$Tag)
    $inp = Join-Path $work "$Tag.mkv"
    [System.IO.File]::WriteAllBytes($inp, $Data)
    $out = Join-Path $work "$Tag.mp4"
    $err = Join-Path $work "$Tag.err"
    cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -f mp4 pipe:1 < `"$inp`" > `"$out`" 2> `"$err`""
    $sz = if (Test-Path $out) { (Get-Item $out).Length } else { 0 }
    Write-Output "  $Label"
    Write-Output "    输入 $($Data.Length) 字节 → 产物 $sz 字节"
    if ($sz -gt 0) {
        # 验证产物真的可解码，而不只是有长度（缺陷 #9 教训）
        $probe = cmd /c "ffprobe -hide_banner -v quiet -print_format json -show_format -show_streams `"$out`" 2>nul"
        try {
            $o = $probe | ConvertFrom-Json
            $vs = $o.streams | Where-Object { $_.codec_type -eq 'video' } | Select-Object -First 1
            Write-Output "    ✅ 可解析: duration=$($o.format.duration) video=$($vs.codec_name) $($vs.width)x$($vs.height) 流数=$($o.streams.Count)"
            # 再进一步：真解码出帧才算数
            $png = Join-Path $work "$Tag.png"
            cmd /c "ffmpeg -hide_banner -v quiet -i `"$out`" -frames:v 1 -f image2 `"$png`" -y 2>nul"
            if (Test-Path $png) {
                $psz = (Get-Item $png).Length
                Write-Output "    ✅ 真解码出帧: $psz 字节 PNG"
            } else {
                Write-Output "    ⚠️ 无法解码出帧"
            }
        } catch {
            Write-Output "    ❌ 产物无法被 ffprobe 解析"
        }
    }
    $e = Get-Content $err -Raw -ErrorAction SilentlyContinue
    if ($e) {
        $lines = ($e -split "`n" | Where-Object {
            $_ -match 'Error|Invalid|error|failed|Cannot|could not|No such|Truncat'
        } | Select-Object -First 3)
        foreach ($l in $lines) { Write-Output "    stderr: $($l.Trim())" }
    }
    Write-Output ''
}

# ---------- Q7：只喂中间的 cluster（无头部）----------
if ($clusterOffsets.Count -ge 3) {
    Write-Output '--- Q7：只喂中间 Cluster（不带文件头）---'
    # 模拟真实 seek：跳到约 30 秒处（60 个 cluster 的第 30 个）
    $midIdx = 30
    $start = $clusterOffsets[$midIdx]
    $end = $clusterOffsets[[Math]::Min($midIdx + 3, $clusterOffsets.Count - 1)]
    $seg = New-Object byte[] ($end - $start)
    [Array]::Copy($bytes, $start, $seg, 0, $end - $start)
    Test-Remux -Label "第 $midIdx 个 Cluster [$start, $end)（无头部）" -Data $seg -Tag 'q7'

    # ---------- Q8/Q9：头部 + 中间 cluster 拼接 ----------
    Write-Output '--- Q8/Q9：文件头 + 中间 Cluster 拼接（这是 P2 seek 的关键）---'
    $hdrLen = $clusterOffsets[0]
    $joined = New-Object byte[] ($hdrLen + ($end - $start))
    [Array]::Copy($bytes, 0, $joined, 0, $hdrLen)
    [Array]::Copy($bytes, $start, $joined, $hdrLen, $end - $start)
    Test-Remux -Label "头部($hdrLen B) + 第 $midIdx 个 Cluster" -Data $joined -Tag 'q9'

    # ---------- 对照：头部 + 从第一个 cluster 开始的等长数据 ----------
    Write-Output '--- 对照：头部 + 首个 Cluster 起的等长数据（应当成功）---'
    $len2 = $end - $start
    $avail = [Math]::Min($len2, $bytes.Length - $clusterOffsets[0])
    $joined2 = New-Object byte[] ($hdrLen + $avail)
    [Array]::Copy($bytes, 0, $joined2, 0, $hdrLen)
    [Array]::Copy($bytes, $clusterOffsets[0], $joined2, $hdrLen, $avail)
    Test-Remux -Label "头部 + 首个 Cluster 起 $avail 字节" -Data $joined2 -Tag 'ctrl'
}

# ---------- Q11：截断的文件（模拟只下载了前半部分）----------
Write-Output '--- Q11：前缀截断（前 50%）能否 remux ---'
$half = New-Object byte[] ([int]($bytes.Length / 2))
[Array]::Copy($bytes, 0, $half, 0, $half.Length)
Test-Remux -Label "前 50% 字节" -Data $half -Tag 'q11'

Write-Output "产物保留在: $work"
