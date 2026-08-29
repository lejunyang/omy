# 端到端验证：真实媒体文件走完整加密流程，回读三个媒体 TLV。
#
# 为什么必须独立于单元测试：
# 单测和 examples/verify_media.rs 都在库内部调用 API。这里用**真实编译出的
# 二进制**走 encrypt → info → decrypt，验证 CLI 参数解析、媒体 TLV 落盘、
# 解密还原这条完整链路。omy-cli 的 `cat --range` off-by-one 正是只有
# 端到端才能发现的那类缺陷。
#
# 前置：pwsh -File spikes/make-media-fixtures.ps1
#       cargo build --release -p omy-cli

$ErrorActionPreference = 'Continue'
$repo = 'E:\Projects\omy'
$omy  = Join-Path $repo 'target\release\omy.exe'
$fix  = Join-Path $repo 'spikes\fixtures\media'
$work = Join-Path $env:TEMP ('omy_media_e2e_' + [guid]::NewGuid().ToString('N').Substring(0,8))

$script:pass = 0
$script:fail = @()

function Check {
    param([bool]$Cond, [string]$Label)
    if ($Cond) {
        $script:pass++
        Write-Output "  PASS  $Label"
    } else {
        $script:fail += $Label
        Write-Output "  FAIL  $Label"
    }
}

function CheckEq {
    param($Got, $Want, [string]$Label)
    if ($Got -eq $Want) {
        $script:pass++
        Write-Output "  PASS  $Label"
    } else {
        $script:fail += "$Label（期望 $Want，实际 $Got）"
        Write-Output "  FAIL  $Label  期望 $Want，实际 $Got"
    }
}

Write-Output '=== omy 媒体 TLV 端到端验证（真实二进制）==='
if (-not (Test-Path $omy)) {
    Write-Output "找不到 $omy，请先 cargo build --release -p omy-cli"
    exit 2
}
# 源码比二进制新就是在测陈旧产物。曾因此追出一条假失败，这里直接拦住。
$newestSrc = Get-ChildItem (Join-Path $repo 'crates') -Recurse -Filter *.rs |
             Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($null -ne $newestSrc -and $newestSrc.LastWriteTime -gt (Get-Item $omy).LastWriteTime) {
    Write-Output "FAIL: 源码比二进制新，测的是陈旧产物"
    Write-Output "  最新源码: $($newestSrc.Name) @ $($newestSrc.LastWriteTime.ToString('MM-dd HH:mm:ss'))"
    Write-Output "请先 cargo build --release -p omy-cli"
    exit 2
}
if (-not (Test-Path $fix)) {
    Write-Output "找不到素材目录，请先运行 spikes/make-media-fixtures.ps1"
    exit 2
}
New-Item -ItemType Directory -Path $work -Force | Out-Null
Write-Output "工作目录: $work"
Write-Output ''

$env:OMY_PASSWORD = 'e2e-test-password'
$PW = @('--password-env', 'OMY_PASSWORD')
# 用最弱 KDF：这是功能验证，不是性能测试
$KDF = @('--kdf-profile', 'mobile')

# ---------- 1. MP4：三个 TLV 应全部写入 ----------
Write-Output '--- 1. MP4 加密后应含 media_meta + moov_cache + thumbnail ---'
$src = Join-Path $fix 'h264_aac.mp4'
$out = Join-Path $work 'video.omy'
& $omy encrypt $src -o $out @PW @KDF 2>&1 | Out-Null
Check (Test-Path $out) '加密产出文件'

if (Test-Path $out) {
    $j = & $omy info $out --json @PW 2>$null | ConvertFrom-Json
    Check ($null -ne $j) 'info --json 可解析'
    if ($null -ne $j) {
        Check ($j.has_thumbnail -eq $true) 'has_thumbnail 为真'
        Check ($j.has_moov_cache -eq $true) 'has_moov_cache 为真'
        Check ($null -ne $j.media) 'media 段存在'
        if ($null -ne $j.media) {
            CheckEq $j.media.playback_tier 'P1' 'MP4+H264+AAC 判为 P1'
            Check ($j.media.duration_ms -gt 7000 -and $j.media.duration_ms -lt 9000) `
                  "时长约 8 秒（实际 $($j.media.duration_ms) ms）"
            CheckEq $j.media.video.codec 'h264' '视频编码为 h264'
            CheckEq $j.media.video.width 320 '宽度 320'
            CheckEq $j.media.audio_tracks 1 '一条音轨'
            Check (-not [string]::IsNullOrWhiteSpace($j.media.tier_reason)) '分级理由非空'
        }
    }
}

# ---------- 2. 解密必须 bit-for-bit 还原 ----------
# 媒体 TLV 是纯附加信息，绝不能影响可还原性
Write-Output ''
Write-Output '--- 2. 加了媒体 TLV 后仍须 bit-for-bit 还原 ---'
$back = Join-Path $work 'restored.mp4'
& $omy decrypt $out -o $back @PW 2>&1 | Out-Null
if (Test-Path $back) {
    $h1 = (Get-FileHash $src -Algorithm SHA256).Hash
    $h2 = (Get-FileHash $back -Algorithm SHA256).Hash
    CheckEq $h2 $h1 '解密结果与原文件 SHA-256 相同'
} else {
    Check $false '解密产出文件'
}

# ---------- 3. MKV 应判为 P2 ----------
Write-Output ''
Write-Output '--- 3. MKV 分级应为 P2（容器不被 video 支持但可 remux）---'
$mkvOut = Join-Path $work 'video.mkv.omy'
& $omy encrypt (Join-Path $fix 'h264_aac.mkv') -o $mkvOut @PW @KDF 2>&1 | Out-Null
if (Test-Path $mkvOut) {
    $jm = & $omy info $mkvOut --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $jm -and $null -ne $jm.media) {
        CheckEq $jm.media.playback_tier 'P2' 'MKV 判为 P2'
        Check ($jm.has_moov_cache -eq $false) 'MKV 不该有 moov 缓存'
    } else {
        Check $false 'MKV 的 media 段存在'
    }
}

# ---------- 4. 纯音频 ----------
Write-Output ''
Write-Output '--- 4. 纯音频：有 meta 无 moov，且不应因抽帧失败而报错 ---'
$mp3Out = Join-Path $work 'audio.omy'
$mp3Err = & $omy encrypt (Join-Path $fix 'audio_only.mp3') -o $mp3Out @PW @KDF 2>&1
if (Test-Path $mp3Out) {
    $ja = & $omy info $mp3Out --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $ja) {
        Check ($null -ne $ja.media) '纯音频也有 media 段'
        Check ($ja.has_moov_cache -eq $false) 'MP3 无 moov 缓存'
        if ($null -ne $ja.media) {
            Check ($null -eq $ja.media.video) '纯音频无视频轨'
            CheckEq $ja.media.audio_tracks 1 '一条音轨'
        }
    }
    # 不该有警告输出
    $warned = ($mp3Err | Out-String) -match '缩略图|thumbnail.*fail'
    Check (-not $warned) '纯音频加密不该输出缩略图警告'
}

# ---------- 5. 非媒体文件 ----------
Write-Output ''
Write-Output '--- 5. 非媒体文件：静默跳过，不产生 media 段也不报警告 ---'
$txt = Join-Path $work 'plain.txt'
Set-Content -Path $txt -Value 'hello, this is not a media file at all' -Encoding UTF8
$txtOut = Join-Path $work 'plain.omy'
$txtErr = & $omy encrypt $txt -o $txtOut @PW @KDF 2>&1
if (Test-Path $txtOut) {
    $jt = & $omy info $txtOut --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $jt) {
        Check ($null -eq $jt.media) '非媒体文件无 media 段'
        Check ($jt.has_thumbnail -eq $false) '非媒体文件无缩略图'
        Check ($jt.has_moov_cache -eq $false) '非媒体文件无 moov'
    }
    $noisy = ($txtErr | Out-String) -match '失败|failed|警告|warn'
    Check (-not $noisy) '非媒体文件不该产生警告噪音'
}

# ---------- 6. --thumbnail none ----------
Write-Output ''
Write-Output '--- 6. --thumbnail none 应真正关闭缩略图 ---'
$noThumb = Join-Path $work 'nothumb.omy'
& $omy encrypt $src -o $noThumb @PW @KDF --thumbnail none 2>&1 | Out-Null
if (Test-Path $noThumb) {
    $jn = & $omy info $noThumb --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $jn) {
        Check ($jn.has_thumbnail -eq $false) '--thumbnail none 后确无缩略图'
        # meta 与 moov 仍应保留：它们是独立开关
        Check ($null -ne $jn.media) '--thumbnail none 不影响 media_meta'
        Check ($jn.has_moov_cache -eq $true) '--thumbnail none 不影响 moov_cache'
    }
    # 文件应当更小
    $s1 = (Get-Item $out).Length
    $s2 = (Get-Item $noThumb).Length
    Check ($s2 -lt $s1) "关闭缩略图后文件更小（$s2 < $s1）"
}

# ---------- 7. --no-media-meta / --no-moov-cache ----------
Write-Output ''
Write-Output '--- 7. --no-media-meta 与 --no-moov-cache 独立生效 ---'
$noMeta = Join-Path $work 'nometa.omy'
& $omy encrypt $src -o $noMeta @PW @KDF --no-media-meta 2>&1 | Out-Null
if (Test-Path $noMeta) {
    $jnm = & $omy info $noMeta --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $jnm) {
        Check ($null -eq $jnm.media) '--no-media-meta 后无 media 段'
        Check ($jnm.has_moov_cache -eq $true) '--no-media-meta 不影响 moov'
        Check ($jnm.has_thumbnail -eq $true) '--no-media-meta 不影响缩略图'
    }
}
$noMoov = Join-Path $work 'nomoov.omy'
& $omy encrypt $src -o $noMoov @PW @KDF --no-moov-cache 2>&1 | Out-Null
if (Test-Path $noMoov) {
    $jnv = & $omy info $noMoov --json @PW 2>$null | ConvertFrom-Json
    if ($null -ne $jnv) {
        Check ($jnv.has_moov_cache -eq $false) '--no-moov-cache 后无 moov'
        Check ($null -ne $jnv.media) '--no-moov-cache 不影响 media_meta'
    }
}

# ---------- 8. --thumbnail-frame ----------
Write-Output ''
Write-Output '--- 8. --thumbnail-frame 各种时间写法 ---'
foreach ($tc in @('5', '5.5', '00:05', '00:00:05')) {
    $o = Join-Path $work ("frame_" + ($tc -replace '[:.]', '_') + '.omy')
    & $omy encrypt $src -o $o @PW @KDF --thumbnail-frame $tc 2>&1 | Out-Null
    if (Test-Path $o) {
        $jf = & $omy info $o --json @PW 2>$null | ConvertFrom-Json
        Check ($jf.has_thumbnail -eq $true) "时间写法 '$tc' 可用"
    } else {
        Check $false "时间写法 '$tc' 可用"
    }
}

# 非法时间点必须报错，而不是静默取 0 秒
$badOut = Join-Path $work 'bad.omy'
& $omy encrypt $src -o $badOut @PW @KDF --thumbnail-frame 'abc' 2>&1 | Out-Null
Check ($LASTEXITCODE -ne 0) '非法时间点应导致非零退出码'
Check (-not (Test-Path $badOut)) '非法时间点不该产出文件'

# 冲突组合必须报错
$conflict = Join-Path $work 'conflict.omy'
& $omy encrypt $src -o $conflict @PW @KDF --thumbnail none --thumbnail-frame 5 2>&1 | Out-Null
Check ($LASTEXITCODE -ne 0) '--thumbnail none 与 --thumbnail-frame 冲突应报错'

# ---------- 9. 未解锁时不泄露媒体信息 ----------
Write-Output ''
Write-Output '--- 9. 不给密码时不得泄露媒体信息 ---'
$jl = & $omy info $out --json 2>$null | ConvertFrom-Json
if ($null -ne $jl) {
    Check ($null -eq $jl.media) '未解锁时 media 为 null'
    # 关键：未解锁时 has_moov_cache 应为 null 而非 false。
    # 「不知道」与「没有」是两件事，脚本要能区分
    Check ($null -eq $jl.has_moov_cache) '未解锁时 has_moov_cache 为 null 而非 false'
    # has_thumbnail 来自 header flag，本就是公开的
    Check ($jl.has_thumbnail -eq $true) 'has_thumbnail 来自 header flag，无需密码'
}

# ---------- 10. 人类可读输出 ----------
Write-Output ''
Write-Output '--- 10. 人类可读输出应含媒体信息 ---'
$human = & $omy info $out @PW 2>$null | Out-String
Check ($human -match 'P1') '输出含播放分级'
Check ($human -match 'h264') '输出含视频编码'
Check ($human -match '0:08') "输出含格式化时长"
Check ($human -match 'moov') '输出含 moov 缓存状态'

# ---------- 清理 ----------
Write-Output ''
Write-Output ('=' * 64)
Write-Output "结果: $($script:pass) 通过, $($script:fail.Count) 失败"
if ($script:fail.Count -gt 0) {
    Write-Output ''
    Write-Output '失败明细:'
    foreach ($f in $script:fail) { Write-Output "  - $f" }
}
Write-Output ('=' * 64)

Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item Env:\OMY_PASSWORD -ErrorAction SilentlyContinue

if ($script:fail.Count -gt 0) { exit 1 }
exit 0
