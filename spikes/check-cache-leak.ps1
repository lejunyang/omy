# S5：检查 WebView2 是否把解密后的明文缓存到磁盘。
#
# 方法（对应路线图 §2.1 的要求）：
#   1. 在 spike 里播放并 seek 若干次
#   2. 扫描 WebView2 的缓存目录
#   3. 用已知的明文特征字节在缓存文件里搜索
#   4. 命中即为 L1 泄露
#
# 特征字节的选取很关键：必须是明文里确实存在、且不会因巧合出现的串。
# MP4 的 'ftyp' + 'mp42'/'isom' 品牌标记，以及视频中段的随机字节，
# 都是合适的探针。
param(
    [string]$Phase = 'scan'   # baseline | scan
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$plain = Join-Path $repo 'spikes\fixtures\sample.mp4'
$state = Join-Path $repo 'spikes\fixtures\s5-baseline.json'

if (-not (Test-Path $plain)) {
    Write-Output "缺少明文样本 $plain，请先运行 make-fixture.ps1"
    exit 1
}

# WebView2 为每个应用建独立的 user data 目录，通常在 EXE 同级或 LOCALAPPDATA。
# 全都要查——漏掉一个就可能漏掉泄露。
$roots = @()
foreach ($c in @(
    (Join-Path $repo 'target\release\omy-spike-webview.exe.WebView2'),
    (Join-Path $env:LOCALAPPDATA 'omy-spike-webview'),
    (Join-Path $env:LOCALAPPDATA 'org.omy.spike.webview'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\Edge\User Data\Default\Cache'),
    (Join-Path $env:LOCALAPPDATA 'Temp')
)) {
    if (Test-Path $c) { $roots += $c }
}
# 再按名字模糊搜一遍 LOCALAPPDATA，捕获未预料的目录名
$extra = Get-ChildItem $env:LOCALAPPDATA -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match 'omy|spike|EBWebView' }
foreach ($e in $extra) { if ($roots -notcontains $e.FullName) { $roots += $e.FullName } }

Write-Output '=== 待检查的目录 ==='
if ($roots.Count -eq 0) { Write-Output '（未找到任何 WebView2 数据目录）' }
foreach ($r in $roots) { Write-Output "  $r" }

if ($Phase -eq 'baseline') {
    $snap = @{}
    foreach ($r in $roots) {
        Get-ChildItem $r -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
            $snap[$_.FullName] = $_.Length
        }
    }
    $snap | ConvertTo-Json -Depth 3 -Compress | Set-Content $state -Encoding UTF8
    Write-Output ''
    Write-Output ("基线已记录：{0} 个文件 → {1}" -f $snap.Count, $state)
    exit 0
}

# ---- scan 阶段 ----
Write-Output ''
Write-Output '=== 构造明文探针 ==='
$bytes = [System.IO.File]::ReadAllBytes($plain)
Write-Output ("明文长度 {0:N0} 字节" -f $bytes.Length)

# 探针 1：文件头（ftyp box）——若缓存了开头，这个必然出现
$probe1 = $bytes[4..19]
# 探针 2/3：中段与尾段各取 16 字节，避开全零区域
function Get-Probe([byte[]]$b, [int]$at) {
    for ($i = $at; $i -lt [Math]::Min($at + 4096, $b.Length - 16); $i++) {
        $slice = $b[$i..($i + 15)]
        # 要求足够"随机"：不同字节数 >= 10，避免选到重复填充
        if (($slice | Select-Object -Unique).Count -ge 10) { return $slice }
    }
    return $null
}
$probe2 = Get-Probe $bytes ([int]($bytes.Length * 0.5))
$probe3 = Get-Probe $bytes ([int]($bytes.Length * 0.85))

$probes = @()
if ($probe1) { $probes += , @{ name = '文件头 ftyp'; data = $probe1 } }
if ($probe2) { $probes += , @{ name = '中段 50%';    data = $probe2 } }
if ($probe3) { $probes += , @{ name = '尾段 85%';    data = $probe3 } }

foreach ($p in $probes) {
    $hex = ($p.data | ForEach-Object { $_.ToString('x2') }) -join ' '
    Write-Output ("  {0,-12} {1}" -f $p.name, $hex)
}

# 载入基线以只查新增/变化的文件
$baseline = @{}
if (Test-Path $state) {
    $obj = Get-Content $state -Raw | ConvertFrom-Json
    foreach ($prop in $obj.PSObject.Properties) { $baseline[$prop.Name] = $prop.Value }
    Write-Output ""
    Write-Output ("已载入基线：{0} 个文件" -f $baseline.Count)
}

Write-Output ''
Write-Output '=== 扫描缓存文件 ==='
$targets = @()
foreach ($r in $roots) {
    Get-ChildItem $r -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        # 只查新增或变大的文件；同时限制单文件 200 MB 以内避免卡死
        $isNew = -not $baseline.ContainsKey($_.FullName)
        $grew  = (-not $isNew) -and ($_.Length -ne $baseline[$_.FullName])
        if (($isNew -or $grew) -and $_.Length -gt 0 -and $_.Length -lt 200MB) {
            $targets += $_
        }
    }
}
Write-Output ("新增/变化的文件：{0} 个" -f $targets.Count)

function Find-Bytes([byte[]]$hay, [byte[]]$needle) {
    if ($needle.Length -eq 0 -or $hay.Length -lt $needle.Length) { return -1 }
    $first = $needle[0]
    $limit = $hay.Length - $needle.Length
    for ($i = 0; $i -le $limit; $i++) {
        if ($hay[$i] -ne $first) { continue }
        $ok = $true
        for ($j = 1; $j -lt $needle.Length; $j++) {
            if ($hay[$i + $j] -ne $needle[$j]) { $ok = $false; break }
        }
        if ($ok) { return $i }
    }
    return -1
}

$hits = @()
$scanned = 0
foreach ($f in $targets) {
    try {
        $content = [System.IO.File]::ReadAllBytes($f.FullName)
    } catch { continue }
    $scanned++
    foreach ($p in $probes) {
        $at = Find-Bytes $content $p.data
        if ($at -ge 0) {
            $hits += [pscustomobject]@{
                File   = $f.FullName
                Probe  = $p.name
                Offset = $at
                Size   = $f.Length
            }
        }
    }
}

Write-Output ("实际读取扫描：{0} 个文件" -f $scanned)
Write-Output ''
Write-Output ('=' * 64)
Write-Output 'Spike S5 结果'
Write-Output ('=' * 64)
if ($hits.Count -eq 0) {
    Write-Output '✅ S5 通过：在所有新增/变化的缓存文件中'
    Write-Output '   均未发现明文特征字节。'
    Write-Output ''
    Write-Output '   前置条件：响应头已带 Cache-Control: no-store, no-cache。'
    Write-Output '   注意这只证明「加了 no-store 后不落盘」，'
    Write-Output '   不证明「不加也不落盘」——产品代码必须保留这些头。'
} else {
    Write-Output ("❌ S5 未通过：发现 {0} 处明文泄露！" -f $hits.Count)
    $hits | Format-Table -AutoSize | Out-String -Width 160 | Write-Output
    Write-Output '   必须排查响应头是否真的生效，并考虑改用一次性 token + 内存缓存。'
}
Write-Output ('=' * 64)
if ($hits.Count -gt 0) { exit 1 }
