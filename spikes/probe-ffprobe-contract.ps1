# 核实 ffprobe 的真实输出结构：omy-media 要解析它，字段名与嵌套必须实测确认，
# 不能凭印象写解析器。同时验证「从 stdin 管道喂数据」这条路可行——
# 文档 §14 要求 FFmpeg 子进程无文件系统访问，只能走管道。
$ErrorActionPreference = 'Continue'
$repo = 'E:\Projects\omy'
$ff   = Join-Path $repo 'tools\ffmpeg\bin\ffprobe.exe'
$mp4  = Join-Path $repo 'spikes\fixtures\sample.mp4'

if (-not (Test-Path $ff))  { Write-Output "缺少 $ff";  exit 1 }
if (-not (Test-Path $mp4)) { Write-Output "缺少 $mp4"; exit 1 }

Write-Output '=== 1. ffprobe 版本 ==='
& $ff -version | Select-Object -First 1

Write-Output ''
Write-Output '=== 2. JSON 输出的完整结构（决定解析器的字段名）==='
& $ff -v quiet -print_format json -show_format -show_streams $mp4

Write-Output ''
Write-Output '=== 3. 关键：能否从 stdin 管道读取（文档 §14 要求无文件系统访问）==='
# 用 - 表示从 stdin 读。注意 ffprobe 对管道输入无法 seek，
# 若 moov 在尾部会失败——这正是需要实测确认的点。
$tmpOut = Join-Path $env:TEMP 'omy_probe_pipe.json'
$proc = Start-Process -FilePath $ff `
    -ArgumentList @('-v','quiet','-print_format','json','-show_format','-show_streams','-') `
    -RedirectStandardInput $mp4 -RedirectStandardOutput $tmpOut `
    -NoNewWindow -PassThru -Wait
Write-Output "退出码: $($proc.ExitCode)"
if (Test-Path $tmpOut) {
    $txt = Get-Content $tmpOut -Raw
    if ($txt.Trim()) {
        Write-Output "管道模式输出长度: $($txt.Length) 字符"
        try {
            $j = $txt | ConvertFrom-Json
            Write-Output "  container = $($j.format.format_name)"
            Write-Output "  duration  = $($j.format.duration)"
            # 管道模式下 size 常为 N/A，这点必须知道
            Write-Output "  size      = $($j.format.size)"
            foreach ($s in $j.streams) {
                Write-Output "  stream #$($s.index) $($s.codec_type) $($s.codec_name)"
            }
        } catch { Write-Output "  JSON 解析失败: $_" }
    } else {
        Write-Output '管道模式无输出（可能因无法 seek 而失败）'
    }
    Remove-Item $tmpOut -Force -ErrorAction SilentlyContinue
}

Write-Output ''
Write-Output '=== 4. moov box 的定位方式（-show_packets 太重，试 -show_entries）==='
# faststart 的 mp4 里 moov 在前面。我们需要知道它的偏移与长度以缓存进 TLV。
# ffprobe 不直接给 box 偏移，需自己解析 box 树——先确认 box 结构。
$bytes = [System.IO.File]::ReadAllBytes($mp4)
$pos = 0
Write-Output '顶层 box 列表（自己解析，确认 moov 位置与长度）：'
while ($pos -lt $bytes.Length -and $pos -lt 20000000) {
    if ($pos + 8 -gt $bytes.Length) { break }
    # box size 是大端 32 位
    $size = [uint32]$bytes[$pos] * 16777216 + [uint32]$bytes[$pos+1] * 65536 +
            [uint32]$bytes[$pos+2] * 256 + [uint32]$bytes[$pos+3]
    $type = [System.Text.Encoding]::ASCII.GetString($bytes, $pos + 4, 4)
    Write-Output ("  offset={0,-10} size={1,-10} type={2}" -f $pos, $size, $type)
    if ($size -le 0) { break }
    $pos += $size
}

Write-Output ''
Write-Output '=== 5. 抽取指定时间点的帧为图片（缩略图用）==='
$thumb = Join-Path $env:TEMP 'omy_thumb_test.webp'
$ffmpeg = Join-Path $repo 'tools\ffmpeg\bin\ffmpeg.exe'
& $ffmpeg -v error -y -ss 12 -i $mp4 -frames:v 1 -vf 'scale=320:-1' -f webp $thumb 2>&1 |
    Select-Object -First 5
if (Test-Path $thumb) {
    Write-Output "缩略图生成成功: $((Get-Item $thumb).Length) 字节"
    Remove-Item $thumb -Force -ErrorAction SilentlyContinue
} else {
    Write-Output '缩略图生成失败'
}
