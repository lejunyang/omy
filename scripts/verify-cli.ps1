#!/usr/bin/env pwsh
# omy CLI 端到端验证。
#
# 用真实编译出的二进制走完整流程，覆盖设计文档 09 号定义的退出码契约。
# 单元测试无法验证「命令行参数解析 + 进程退出码 + stdout/stderr 分工」，
# 这些只能靠真实调用二进制来验证。
#
# 用法：pwsh -File scripts/verify-cli.ps1
# 前置：cargo build -p omy-cli

$ErrorActionPreference = 'Continue'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$exe = Join-Path $repo 'target\debug\omy.exe'
if (-not (Test-Path $exe)) {
    Write-Output "FAIL: 未找到 $exe"
    Write-Output "请先运行: cargo build -p omy-cli"
    exit 1
}

$dir = Join-Path $env:TEMP ('omy_cli_' + [guid]::NewGuid().ToString('N').Substring(0,8))
New-Item -ItemType Directory -Path $dir -Force | Out-Null
Write-Output "测试目录: $dir"
Write-Output ""

$pass = 0
$fail = 0
$details = @()

function Check($cond, $label) {
    if ($cond) {
        $script:pass++
        Write-Output "  PASS  $label"
    } else {
        $script:fail++
        $script:details += $label
        Write-Output "  FAIL  $label"
    }
}

# 用密码文件而非命令行参数——CLI 本身就拒绝 --password
$pwFile = Join-Path $dir 'pw.txt'
Set-Content -Path $pwFile -Value 'correct-horse-battery' -NoNewline
$pwFile2 = Join-Path $dir 'pw2.txt'
Set-Content -Path $pwFile2 -Value 'a-different-password' -NoNewline

# ============ 1. 拒绝明文密码参数（旁路 L12）============
Write-Output "=== 1. 必须拒绝 --password 明文参数 ==="
$o = & $exe encrypt --password secret nosuchfile.txt 2>&1 | Out-String
Check ($LASTEXITCODE -eq 2) "退出码为 2（参数错误），实际 $LASTEXITCODE"
Check ($o -match 'ps aux') "解释中提到 ps aux 风险"
Check ($o -match '--password-stdin') "给出了 --password-stdin 替代方案"
Check ($o -match '--password-file') "给出了 --password-file 替代方案"
$o2 = & $exe encrypt --password=secret nosuchfile.txt 2>&1 | Out-String
Check ($LASTEXITCODE -eq 2) "--password=x 形式同样被拒绝"

# ============ 2. 加密 → 信息 → 解密 往返 ============
Write-Output ""
Write-Output "=== 2. 加密/解密往返 ==="
$src = Join-Path $dir 'hello.txt'
$content = "Hello, omy! 中文内容测试 " + ('x' * 5000)
Set-Content -Path $src -Value $content -NoNewline -Encoding UTF8
$srcBytes = [System.IO.File]::ReadAllBytes($src)

& $exe encrypt --password-file $pwFile --kdf-profile mobile $src 2>&1 | Out-Null
$encFile = "$src.omy"
Check ($LASTEXITCODE -eq 0) "encrypt 退出码 0"
Check (Test-Path $encFile) "生成了 $([System.IO.Path]::GetFileName($encFile))"

if (Test-Path $encFile) {
    # 密文里不应出现明文特征
    $encBytes = [System.IO.File]::ReadAllBytes($encFile)
    $encText = [System.Text.Encoding]::ASCII.GetString($encBytes)
    Check (-not $encText.Contains('Hello, omy')) "密文中不含明文片段"
    Check ($encBytes[0] -eq 0x4F -and $encBytes[1] -eq 0x4D -and $encBytes[2] -eq 0x59) "文件头以 OMY 开始"

    # info 不需要密码
    $info = & $exe info $encFile 2>&1 | Out-String
    Check ($LASTEXITCODE -eq 0) "info 退出码 0"
    Check ($info -match 'OMYFILE') "info 显示格式标识"
    Check ($info -match 'Argon2id') "info 显示 KDF"
    # 关键：绝不能泄露 slot 数量
    Check ($info -notmatch '(?m)^\s*Slot.*[1-8]\s*$') "info 不泄露 slot 实际数量"

    # info --json 可被解析
    $j = & $exe --json info $encFile 2>&1 | Out-String
    $ok = $false
    try { $obj = $j | ConvertFrom-Json; $ok = $true } catch { $ok = $false }
    Check $ok "info --json 输出合法 JSON"
    if ($ok) {
        Check ($obj.format -eq 'OMYFILE') "JSON format 字段正确"
        Check ($null -eq $obj.slot_count) "JSON slot_count 为 null（不可探测）"
        Check ($obj.chunk_count -ge 1) "JSON chunk_count 合理"
    }

    # 解密到新位置
    $outFile = Join-Path $dir 'restored.txt'
    & $exe decrypt --password-file $pwFile -o $outFile $encFile 2>&1 | Out-Null
    Check ($LASTEXITCODE -eq 0) "decrypt 退出码 0"
    if (Test-Path $outFile) {
        $restored = [System.IO.File]::ReadAllBytes($outFile)
        $same = ($restored.Length -eq $srcBytes.Length)
        if ($same) {
            for ($i = 0; $i -lt $restored.Length; $i++) {
                if ($restored[$i] -ne $srcBytes[$i]) { $same = $false; break }
            }
        }
        Check $same "解密结果与原文件逐字节一致"
    } else {
        Check $false "解密输出文件不存在"
    }
}

# ============ 3. 退出码契约 ============
Write-Output ""
Write-Output "=== 3. 退出码契约（脚本据此区分错误类型）==="
# 错误密码 → 3
& $exe decrypt --password-file $pwFile2 --verify-only $encFile 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 3) "密码错误 → 退出码 3，实际 $LASTEXITCODE"

# 篡改文件 → 4
$tampered = Join-Path $dir 'tampered.omy'
Copy-Item $encFile $tampered
$tb = [System.IO.File]::ReadAllBytes($tampered)
$tb[$tb.Length - 20] = $tb[$tb.Length - 20] -bxor 0xFF
[System.IO.File]::WriteAllBytes($tampered, $tb)
& $exe decrypt --password-file $pwFile --verify-only $tampered 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 4) "载荷被篡改 → 退出码 4，实际 $LASTEXITCODE"

# 非 omy 文件 → 4（BadMagic 归为损坏）
$notOmy = Join-Path $dir 'plain.omy'
Set-Content -Path $notOmy -Value 'this is not an omy file at all' -NoNewline
& $exe info $notOmy 2>&1 | Out-Null
Check ($LASTEXITCODE -ne 0) "非 omy 文件 info 失败（退出码 $LASTEXITCODE）"

# JSON 模式下错误也是 JSON
$ej = & $exe --json decrypt --password-file $pwFile2 --verify-only $encFile 2>&1 | Out-String
$ejOk = $false
try { $eo = $ej | ConvertFrom-Json; $ejOk = $true } catch {}
Check $ejOk "错误在 --json 下也输出合法 JSON"
if ($ejOk) {
    Check ($eo.error.code -eq 'WRONG_PASSWORD') "错误 code 为英文常量 WRONG_PASSWORD"
    Check ($eo.error.exit_code -eq 3) "错误 JSON 中 exit_code 为 3"
}

# ============ 4. cat 管道输出 ============
Write-Output ""
Write-Output "=== 4. cat 流式输出到 stdout ==="
$catOut = Join-Path $dir 'cat.bin'
# 用 cmd 重定向确保拿到纯二进制
& $exe cat --password-file $pwFile $encFile > $catOut 2>$null
Check ($LASTEXITCODE -eq 0) "cat 退出码 0"
if (Test-Path $catOut) {
    $cb = [System.IO.File]::ReadAllBytes($catOut)
    Check ($cb.Length -eq $srcBytes.Length) "cat 输出长度正确（$($cb.Length) vs $($srcBytes.Length)）"
}
# 范围读取。--range 按 HTTP 惯例是闭区间：0-99 应为 100 字节。
# 早期这里写的是 0-100 期望 100 字节，与当时「终点当开区间」的实现同错，
# 于是掩盖了每次少读一字节的缺陷（Spike S1 里视频报 code=4 才暴露）。
$catRange = Join-Path $dir 'catr.bin'
& $exe cat --password-file $pwFile --range 0-99 $encFile > $catRange 2>$null
if (Test-Path $catRange) {
    Check ((Get-Item $catRange).Length -eq 100) "cat --range 0-99 输出 100 字节（闭区间）"
}
# 单字节区间：最容易暴露 off-by-one
$catOne = Join-Path $dir 'cat1.bin'
& $exe cat --password-file $pwFile --range 0-0 $encFile > $catOne 2>$null
if (Test-Path $catOne) {
    Check ((Get-Item $catOne).Length -eq 1) "cat --range 0-0 输出 1 字节"
}
# 末尾区间必须能取到最后一个字节
$catTail = Join-Path $dir 'cattail.bin'
$lastOff = $srcBytes.Length - 1
& $exe cat --password-file $pwFile --range "$lastOff-$lastOff" $encFile > $catTail 2>$null
if (Test-Path $catTail) {
    $tb2 = [System.IO.File]::ReadAllBytes($catTail)
    Check ($tb2.Length -eq 1 -and $tb2[0] -eq $srcBytes[$lastOff]) "cat 能读到最后一个字节"
}
# suffix 形式应取最后 N 字节（与 HTTP Range: bytes=-N 一致）
$catSuffix = Join-Path $dir 'catsuf.bin'
& $exe cat --password-file $pwFile --range -256 $encFile > $catSuffix 2>$null
if (Test-Path $catSuffix) {
    $sb = [System.IO.File]::ReadAllBytes($catSuffix)
    $wantTail = $srcBytes[($srcBytes.Length - 256)..($srcBytes.Length - 1)]
    $tailSame = ($sb.Length -eq 256)
    if ($tailSame) {
        for ($i = 0; $i -lt 256; $i++) {
            if ($sb[$i] -ne $wantTail[$i]) { $tailSame = $false; break }
        }
    }
    Check $tailSame "cat --range -256 取到最后 256 字节"
}

# ============ 5. 多密码 slot ============
Write-Output ""
Write-Output "=== 5. 单文件多密码（可否认性）==="
$multi = Join-Path $dir 'multi.txt'
Set-Content -Path $multi -Value 'shared content' -NoNewline
# 用 stdin 提供第一个密码，无法交互式追加，故此处验证单密码文件的大小一致性
& $exe encrypt --password-file $pwFile --kdf-profile mobile $multi 2>&1 | Out-Null
$single = Join-Path $dir 'single.txt'
Set-Content -Path $single -Value 'shared content' -NoNewline
& $exe encrypt --password-file $pwFile2 --kdf-profile mobile $single 2>&1 | Out-Null
if ((Test-Path "$multi.omy") -and (Test-Path "$single.omy")) {
    $s1 = (Get-Item "$multi.omy").Length
    $s2 = (Get-Item "$single.omy").Length
    Check ($s1 -eq $s2) "不同密码、相同内容 → 文件大小相同（$s1 = $s2）"
}

# ============ 6. list 与 scan ============
Write-Output ""
Write-Output "=== 6. list / scan ==="
$listOut = & $exe list $dir 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0) "list 退出码 0"
Check ($listOut -match 'hello\.txt\.omy') "list 列出了加密文件"

$scanJson = & $exe --json scan --password-file $pwFile --no-recursive $dir 2>&1 | Out-String
$sOk = $false
try { $so = $scanJson | ConvertFrom-Json; $sOk = $true } catch {}
Check $sOk "scan --json 输出合法 JSON"
if ($sOk) {
    Check ($so.stats.omy_found -ge 3) "scan 找到至少 3 个 omy 文件（实际 $($so.stats.omy_found)）"
    Check ($so.stats.unlocked -ge 2) "scan 解锁了属于该密码的文件（实际 $($so.stats.unlocked)）"
    $names = @($so.files | Where-Object { $_.unlocked } | ForEach-Object { $_.filename })
    Check ($names -contains 'hello.txt') "scan 还原出原始文件名 hello.txt"
}

# ============ 7. 分片 ============
Write-Output ""
Write-Output "=== 7. 分片切分/检查/合并 ==="
$big = Join-Path $dir 'big.bin'
$bigBytes = New-Object byte[] 300000
for ($i = 0; $i -lt $bigBytes.Length; $i++) { $bigBytes[$i] = ($i % 251) }
[System.IO.File]::WriteAllBytes($big, $bigBytes)
& $exe encrypt --password-file $pwFile --kdf-profile mobile $big 2>&1 | Out-Null
$bigEnc = "$big.omy"

& $exe shard split --size 100K $bigEnc 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 0) "shard split 退出码 0"
$shards = Get-ChildItem $dir -Filter 'big.bin.omy.*' | Sort-Object Name
Check ($shards.Count -ge 3) "生成了 $($shards.Count) 个分片"

$chk = & $exe shard check $bigEnc 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0) "shard check 完整时退出码 0"
Check ($chk -match '完整') "check 报告完整"
Check ($chk -match '冗余头') "check 报告冗余头状态"

# 删掉中间一片，check 必须报缺片且退出码为 6
if ($shards.Count -ge 3) {
    $victim = $shards[1].FullName
    $backup = "$victim.bak"
    Move-Item $victim $backup
    & $exe shard check $bigEnc 2>&1 | Out-Null
    Check ($LASTEXITCODE -eq 6) "缺片 → 退出码 6，实际 $LASTEXITCODE"
    Move-Item $backup $victim
}

# 合并回来并验证与原加密文件一致
$merged = Join-Path $dir 'merged.omy'
& $exe shard merge -o $merged $bigEnc 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 0) "shard merge 退出码 0"
if (Test-Path $merged) {
    $h1 = (Get-FileHash $bigEnc -Algorithm SHA256).Hash
    $h2 = (Get-FileHash $merged -Algorithm SHA256).Hash
    Check ($h1 -eq $h2) "合并结果与原加密文件哈希一致"
}

# ============ 8. 目录容器 ============
Write-Output ""
Write-Output "=== 8. 目录容器模式 ==="
$folder = Join-Path $dir 'work'
New-Item -ItemType Directory -Path (Join-Path $folder 'docs') -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $folder 'empty') -Force | Out-Null
Set-Content -Path (Join-Path $folder 'docs\report.txt') -Value 'quarterly report' -NoNewline
Set-Content -Path (Join-Path $folder 'readme.md') -Value '# readme 中文' -NoNewline

& $exe encrypt --password-file $pwFile --kdf-profile mobile --mode container $folder 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 0) "容器加密退出码 0"
$folderEnc = "$folder.omy"
Check (Test-Path $folderEnc) "生成容器文件"

if (Test-Path $folderEnc) {
    $ci = & $exe --json info $folderEnc 2>&1 | Out-String
    try {
        $cio = $ci | ConvertFrom-Json
        Check ($cio.is_container -eq $true) "info 识别为容器"
    } catch { Check $false "容器 info JSON 解析失败" }

    $extractTo = Join-Path $dir 'extracted'
    New-Item -ItemType Directory -Path $extractTo -Force | Out-Null
    & $exe decrypt --password-file $pwFile --output-dir $extractTo $folderEnc 2>&1 | Out-Null
    Check ($LASTEXITCODE -eq 0) "容器解密退出码 0"
    Check (Test-Path (Join-Path $extractTo 'work\docs\report.txt')) "还原了嵌套文件"
    Check (Test-Path (Join-Path $extractTo 'work\readme.md')) "还原了根层文件"
    Check (Test-Path (Join-Path $extractTo 'work\empty')) "还原了空目录"
    if (Test-Path (Join-Path $extractTo 'work\docs\report.txt')) {
        $rc = Get-Content (Join-Path $extractTo 'work\docs\report.txt') -Raw
        Check ($rc -eq 'quarterly report') "嵌套文件内容正确"
    }
}

# ============ 9. verify ============
Write-Output ""
Write-Output "=== 9. verify ==="
& $exe verify --password-file $pwFile $encFile 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 0) "verify 正常文件退出码 0"
& $exe verify --password-file $pwFile $tampered 2>&1 | Out-Null
Check ($LASTEXITCODE -ne 0) "verify 篡改文件失败（退出码 $LASTEXITCODE）"

# ============ 10. 工具类命令 ============
Write-Output ""
Write-Output "=== 10. doctor / bench / completion ==="
$doc = & $exe doctor 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0) "doctor 退出码 0"
Check ($doc -match '未实现') "doctor 如实报告未实现的能力"

$bench = & $exe bench --quick --throughput-size 4M 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0) "bench 退出码 0"
Check ($bench -match 'Argon2id') "bench 报告 Argon2 实测"
Check ($bench -match 'GB/s') "bench 报告吞吐"

foreach ($sh in @('bash','zsh','fish','powershell')) {
    $c = & $exe completion $sh 2>&1 | Out-String
    Check ($LASTEXITCODE -eq 0 -and $c.Length -gt 200) "completion $sh 生成脚本（$($c.Length) 字符）"
}

# ============ 11. 帮助与版本 ============
Write-Output ""
Write-Output "=== 11. --help / --version ==="
$h = & $exe --help 2>&1 | Out-String
Check ($LASTEXITCODE -eq 0) "--help 退出码 0"
foreach ($sub in @('encrypt','decrypt','info','verify','list','scan','cat','key','shard','bench','doctor','completion')) {
    Check ($h -match "\b$sub\b") "帮助中列出子命令 $sub"
}
& $exe --version 2>&1 | Out-Null
Check ($LASTEXITCODE -eq 0) "--version 退出码 0"

# ============ 汇总 ============
Write-Output ""
Write-Output ("=" * 64)
Write-Output "结果: $pass 项通过, $fail 项失败"
if ($details.Count -gt 0) {
    Write-Output ""
    Write-Output "失败明细:"
    foreach ($d in $details) { Write-Output "  - $d" }
}
Write-Output ("=" * 64)

Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
if ($fail -gt 0) { exit 1 }
