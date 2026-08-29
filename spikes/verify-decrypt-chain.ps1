# 独立验证：spike 用的解密链路是否真能还原出可播放的 MP4。
#
# 这一步的意义：<video> 报 code=4（MEDIA_ERR_SRC_NOT_SUPPORTED）时，
# 可能是协议层没通，也可能是返回的字节根本不是合法 MP4。
# 必须先用与 WebView 无关的通道排除后者，否则会在错误方向上排查。
$ErrorActionPreference = 'Stop'
$repo  = Split-Path -Parent $PSScriptRoot
$fix   = Join-Path $repo 'spikes\fixtures'
$plain = Join-Path $fix 'sample.mp4'
$enc   = Join-Path $fix 'sample.mp4.omy'
$omy   = Join-Path $repo 'target\release\omy.exe'
$ff    = Join-Path $repo 'tools\ffmpeg\bin\ffprobe.exe'
$pw    = Join-Path $fix 'pw.txt'

Write-Output '=== 1. 用 omy CLI 整体解密并逐字节比对 ==='
$out = Join-Path $fix '_roundtrip.mp4'
if (Test-Path $out) { Remove-Item $out -Force }
& $omy decrypt --password-file $pw -o $out $enc
if ($LASTEXITCODE -ne 0) { Write-Output 'decrypt 失败'; exit 1 }

$h1 = (Get-FileHash $plain -Algorithm SHA256).Hash
$h2 = (Get-FileHash $out   -Algorithm SHA256).Hash
if ($h1 -eq $h2) {
    Write-Output "  整体解密一致 (SHA256 $($h1.Substring(0,16))...)"
} else {
    Write-Output '  整体解密不一致！问题在 core，不在 WebView'
    exit 1
}

Write-Output ''
Write-Output '=== 2. 用 cat --range 模拟 WebView 的分段请求 ==='
# WebView 首次请求通常是 bytes=0-（拿元数据），随后按需 seek。
# 这里模拟几个典型区间，并把拼接结果与明文对应位置比对。
$plainBytes = [System.IO.File]::ReadAllBytes($plain)
$total = $plainBytes.Length
$cases = @(
    @{ label = '开头 64 KiB'; start = 0;                        len = 65536 },
    @{ label = '中段 64 KiB'; start = [int]($total * 0.5);      len = 65536 },
    @{ label = '85% 处 64 KiB'; start = [int]($total * 0.85);   len = 65536 },
    @{ label = '末尾 32 KiB'; start = ($total - 32768);         len = 32768 }
)
$allOk = $true
foreach ($c in $cases) {
    $s = $c.start
    $e = $s + $c.len - 1
    if ($e -ge $total) { $e = $total - 1 }
    $tmp = Join-Path $fix '_seg.bin'
    & $omy cat --password-file $pw --range "$s-$e" $enc > $tmp 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Output "  $($c.label): cat 失败"
        $allOk = $false
        continue
    }
    $got  = [System.IO.File]::ReadAllBytes($tmp)
    $want = $plainBytes[$s..$e]
    $same = ($got.Length -eq $want.Length)
    if ($same) {
        for ($i = 0; $i -lt $got.Length; $i++) {
            if ($got[$i] -ne $want[$i]) { $same = $false; break }
        }
    }
    $mark = if ($same) { 'OK  ' } else { 'FAIL' }
    Write-Output ("  {0} {1,-16} offset={2,-10} {3} 字节" -f $mark, $c.label, $s, $got.Length)
    if (-not $same) { $allOk = $false }
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
}
if (-not $allOk) { Write-Output '分段读取有误，问题在 core'; exit 1 }

Write-Output ''
Write-Output '=== 3. ffprobe 验证解密产物确实是可解码的 MP4 ==='
if (Test-Path $ff) {
    & $ff -v error -show_entries format=format_name,duration,size -show_entries stream=codec_name,codec_type,width,height -of default=noprint_wrappers=1 $out
    if ($LASTEXITCODE -ne 0) { Write-Output '  ffprobe 报错'; exit 1 }
} else {
    Write-Output "  跳过：缺少 $ff"
}

Write-Output ''
Write-Output '=== 4. 检查 MP4 头部品牌（<video> 据此判断能否播放）==='
$head = $plainBytes[0..31]
$ftyp = [System.Text.Encoding]::ASCII.GetString($plainBytes[4..7])
$brand = [System.Text.Encoding]::ASCII.GetString($plainBytes[8..11])
Write-Output "  box=$ftyp  major_brand=$brand"
Write-Output ("  前 32 字节: " + (($head | ForEach-Object { $_.ToString('x2') }) -join ' '))

Remove-Item $out -Force -ErrorAction SilentlyContinue
Write-Output ''
Write-Output '结论：以上全部通过则解密链路无问题，code=4 的原因在 WebView 协议层。'
