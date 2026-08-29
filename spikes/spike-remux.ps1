# Spike：验证纯管道 remux 到 fMP4 是否可行。
#
# 为什么必须先验证：
# 文档 §14 要求 FFmpeg 子进程「无文件系统访问，只通过 stdin/stdout 管道通信」。
# 但缺陷 #7 已经证明管道输入有严重限制——无法 seek，尾部 moov 的 MP4 直接读不了。
# fMP4 输出还有额外问题：常规 MP4 muxer 需要回头改写 moov，管道输出做不到。
#
# 本 spike 要回答四个问题：
#   Q1 管道输入 + 管道输出能否产出 fMP4？需要哪些 flag？
#   Q2 MKV(H.264+AAC) 能否 -c copy 直接 remux（无需重编码）？
#   Q3 产出的 fMP4 是否真的是分片结构（有 moof，可被 MSE 接受）？
#   Q4 能否只 remux 某个时间区间（seek 时按需产片段）？
#
# 任何一问失败，P2 的实现路径就要改，绝不能先写实现再发现走不通。

$ErrorActionPreference = 'Continue'
$repo = 'E:\Projects\omy'
$fix  = Join-Path $repo 'spikes\fixtures\media'
$work = Join-Path $env:TEMP ('omy_remux_spike_' + [guid]::NewGuid().ToString('N').Substring(0,8))

if (-not (Test-Path $fix)) {
    Write-Output "找不到素材目录，请先运行 spikes/make-media-fixtures.ps1"
    exit 2
}
New-Item -ItemType Directory -Path $work -Force | Out-Null

Write-Output '=== Spike：纯管道 remux 到 fMP4 ==='
Write-Output "工作目录: $work"
Write-Output ''

# 解析 MP4 顶层 box，用来确认产物到底是不是分片结构。
# 只看文件大小是不够的——缺陷 #9 的教训：错误文本也有长度。
function Get-Mp4Boxes {
    param([string]$Path, [int]$Max = 20)
    if (-not (Test-Path $Path)) { return @() }
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $boxes = @()
        $pos = 0L
        $len = $fs.Length
        while ($pos -lt $len -and $boxes.Count -lt $Max) {
            if ($pos + 8 -gt $len) { break }
            $fs.Position = $pos
            $hdr = New-Object byte[] 8
            $read = $fs.Read($hdr, 0, 8)
            if ($read -lt 8) { break }
            # box size 是大端 u32
            $size = [uint32]$hdr[0] * 16777216 + [uint32]$hdr[1] * 65536 + `
                    [uint32]$hdr[2] * 256 + [uint32]$hdr[3]
            $type = [System.Text.Encoding]::ASCII.GetString($hdr, 4, 4)
            $boxes += [pscustomobject]@{ Type = $type; Size = $size; Offset = $pos }
            if ($size -lt 8) { break }   # 0 或 1 需特殊处理，这里够用
            $pos += $size
        }
        return $boxes
    } finally { $fs.Close() }
}

function Show-Result {
    param([string]$Label, [string]$Out, [string]$ErrFile)
    $exists = Test-Path $Out
    $size = if ($exists) { (Get-Item $Out).Length } else { 0 }
    Write-Output "  $Label"
    Write-Output "    产物 $size 字节"
    if ($size -gt 0) {
        $boxes = Get-Mp4Boxes -Path $Out
        $types = ($boxes | ForEach-Object { $_.Type }) -join ' '
        Write-Output "    顶层 box: $types"
        $hasMoof = ($boxes | Where-Object { $_.Type -eq 'moof' }).Count -gt 0
        $hasMoov = ($boxes | Where-Object { $_.Type -eq 'moov' }).Count -gt 0
        $hasFtyp = ($boxes | Where-Object { $_.Type -eq 'ftyp' }).Count -gt 0
        Write-Output "    ftyp=$hasFtyp moov=$hasMoov moof=$hasMoof  → 分片结构: $($hasMoof -and $hasMoov)"
    }
    if ((Test-Path $ErrFile) -and (Get-Item $ErrFile).Length -gt 0) {
        $err = Get-Content $ErrFile -Raw
        # 只显示真正的错误行，FFmpeg 的常规输出太吵
        $lines = ($err -split "`n" | Where-Object {
            $_ -match 'Error|error|Invalid|invalid|failed|Cannot|Unable|muxer|not supported'
        } | Select-Object -First 4)
        if ($lines) {
            Write-Output "    stderr 关键行:"
            foreach ($l in $lines) { Write-Output "      $($l.Trim())" }
        }
    }
    Write-Output ''
}

$mkv = Join-Path $fix 'h264_aac.mkv'
Write-Output "输入素材: h264_aac.mkv（$((Get-Item $mkv).Length) 字节）"
Write-Output ''

# ---------- Q1 + Q2：管道进、管道出、-c copy、fMP4 ----------
Write-Output '--- Q1/Q2：管道输入 → 管道输出 → fMP4（-c copy）---'

# 组合 A：只给 movflags，不给 frag_keyframe
$outA = Join-Path $work 'a.mp4'
$errA = Join-Path $work 'a.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags empty_moov -f mp4 pipe:1 < `"$mkv`" > `"$outA`" 2> `"$errA`""
Show-Result -Label 'A: -movflags empty_moov' -Out $outA -ErrFile $errA

# 组合 B：加 frag_keyframe（每个关键帧一个 fragment）
$outB = Join-Path $work 'b.mp4'
$errB = Join-Path $work 'b.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov -f mp4 pipe:1 < `"$mkv`" > `"$outB`" 2> `"$errB`""
Show-Result -Label 'B: -movflags frag_keyframe+empty_moov' -Out $outB -ErrFile $errB

# 组合 C：加 default_base_moof（MSE 推荐，避免某些播放器的偏移歧义）
$outC = Join-Path $work 'c.mp4'
$errC = Join-Path $work 'c.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -f mp4 pipe:1 < `"$mkv`" > `"$outC`" 2> `"$errC`""
Show-Result -Label 'C: +default_base_moof' -Out $outC -ErrFile $errC

# 组合 D：用 -f dash 或 -f mp4 配合 frag_duration（固定时长分片）
$outD = Join-Path $work 'd.mp4'
$errD = Join-Path $work 'd.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -frag_duration 2000000 -f mp4 pipe:1 < `"$mkv`" > `"$outD`" 2> `"$errD`""
Show-Result -Label 'D: +frag_duration 2s' -Out $outD -ErrFile $errD

# ---------- Q3：产物能否被 ffprobe 正确解析 ----------
Write-Output '--- Q3：产物可被 ffprobe 解析（验证不是损坏文件）---'
foreach ($f in @(@('B', $outB), @('C', $outC))) {
    $label = $f[0]; $path = $f[1]
    if ((Test-Path $path) -and (Get-Item $path).Length -gt 0) {
        $pj = cmd /c "ffprobe -hide_banner -v quiet -print_format json -show_format -show_streams `"$path`" 2>nul"
        try {
            $o = $pj | ConvertFrom-Json
            $vs = $o.streams | Where-Object { $_.codec_type -eq 'video' } | Select-Object -First 1
            $as = $o.streams | Where-Object { $_.codec_type -eq 'audio' } | Select-Object -First 1
            Write-Output "  $label : format=$($o.format.format_name) duration=$($o.format.duration)"
            Write-Output "      video=$($vs.codec_name) $($vs.width)x$($vs.height)  audio=$($as.codec_name)"
        } catch {
            Write-Output "  $label : ffprobe 无法解析（产物可能损坏）"
        }
    }
}
Write-Output ''

# ---------- Q4：能否只 remux 某个时间区间 ----------
Write-Output '--- Q4：区间 remux（seek 场景：只产出 2s–5s）---'
# 注意 -ss 放在 -i 之前是输入 seek，管道输入下能否工作是关键
$outE = Join-Path $work 'e.mp4'
$errE = Join-Path $work 'e.err'
cmd /c "ffmpeg -hide_banner -ss 2 -i pipe:0 -t 3 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -f mp4 pipe:1 < `"$mkv`" > `"$outE`" 2> `"$errE`""
Show-Result -Label 'E: -ss 2 -t 3（输入 seek）' -Out $outE -ErrFile $errE

$outF = Join-Path $work 'f.mp4'
$errF = Join-Path $work 'f.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -ss 2 -t 3 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -f mp4 pipe:1 < `"$mkv`" > `"$outF`" 2> `"$errF`""
Show-Result -Label 'F: -i 后 -ss 2 -t 3（输出 seek）' -Out $outF -ErrFile $errF

# ---------- 附加：init segment 能否单独产出 ----------
# MSE 需要先 append init segment（ftyp+moov），再 append media segment（moof+mdat）
Write-Output '--- Q5：init segment 能否单独产出（MSE 必需）---'
$outG = Join-Path $work 'init.mp4'
$errG = Join-Path $work 'g.err'
cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov+default_base_moof -f mp4 -frames:v 0 -frames:a 0 pipe:1 < `"$mkv`" > `"$outG`" 2> `"$errG`""
Show-Result -Label 'G: -frames 0（只出 init）' -Out $outG -ErrFile $errG

# ---------- 附加：不可 remux 的场景 ----------
Write-Output '--- Q6：FLAC in MKV（音轨进不了 MP4）应当明确失败 ---'
$flac = Join-Path $fix 'h264_flac.mkv'
if (Test-Path $flac) {
    $outH = Join-Path $work 'h.mp4'
    $errH = Join-Path $work 'h.err'
    cmd /c "ffmpeg -hide_banner -i pipe:0 -c copy -movflags frag_keyframe+empty_moov -f mp4 pipe:1 < `"$flac`" > `"$outH`" 2> `"$errH`""
    Show-Result -Label 'H: H.264+FLAC → MP4（-c copy）' -Out $outH -ErrFile $errH
}

Write-Output '=== 素材与产物尺寸对照 ==='
Write-Output "原 MKV        : $((Get-Item $mkv).Length)"
foreach ($p in @(@('A',$outA),@('B',$outB),@('C',$outC),@('D',$outD),@('E',$outE),@('F',$outF),@('G',$outG))) {
    if (Test-Path $p[1]) {
        Write-Output "$($p[0]) : $((Get-Item $p[1]).Length)"
    }
}

Write-Output ''
Write-Output "产物保留在: $work"
Write-Output '（人工核对后可手动删除）'
