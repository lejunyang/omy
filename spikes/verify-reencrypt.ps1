# 验证 key reencrypt：真正的密钥轮换。
#
# 与 verify-key-slots.ps1 的分工：那个验「改密码不动载荷」，这个验
# 「轮换确实换了文件密钥」。两件事的断言方向正好相反，混在一个脚本里
# 容易把「载荷未变」和「载荷必须变」写串。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$cli = Join-Path $root 'target\debug\omy.exe'
$pass = 0; $fail = 0

function check($name, $ok, $detail = '') {
    if ($ok) { $script:pass++; Write-Output "  PASS  $name  $detail" }
    else { $script:fail++; Write-Output "  FAIL  $name  $detail" }
}

if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }
$newest = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newest.LastWriteTime -gt (Get-Item $cli).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newest.Name)），先 cargo build -p omy-cli"
    exit 1
}

$work = Join-Path $env:TEMP "omy-reenc-$PID"
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

try {
    $env:PW_A = 'pw-alpha'; $env:PW_B = 'pw-beta'; $env:PW_C = 'pw-gamma'

    Write-Output ''
    Write-Output '=== 1. 轮换后旧密码作废、明文不变 ==='
    $src = Join-Path $work 'doc.txt'
    # 造一个跨多个块的载荷，确保重新加密真的走了分块循环
    $content = ('omy reencrypt payload line' * 40) -join ''
    1..200 | ForEach-Object { Add-Content -LiteralPath $src -Value $content -Encoding UTF8 }
    $plainBefore = [IO.File]::ReadAllBytes($src)
    $f = Join-Path $work 'doc.omy'
    & $cli encrypt $src -o $f --password-env PW_A --kdf-profile mobile --yes *> $null
    check '造出加密文件' (Test-Path $f) "$((Get-Item $f).Length) B"

    $bytesBefore = [IO.File]::ReadAllBytes($f)
    $uuidBefore = ($bytesBefore[16..31] | ForEach-Object { $_.ToString('x2') }) -join ''
    $sizeBefore = $bytesBefore.Length

    $out = & $cli key reencrypt $f --password-env PW_A --new-password-env PW_B --yes 2>&1
    check 'reencrypt 执行成功' ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"
    check '输出说明载荷已重写' (($out -join ' ') -match '已重写') ''

    $bytesAfter = [IO.File]::ReadAllBytes($f)
    $uuidAfter = ($bytesAfter[16..31] | ForEach-Object { $_.ToString('x2') }) -join ''
    check 'file_uuid 已更换' ($uuidBefore -ne $uuidAfter) "$($uuidBefore.Substring(0,8)) -> $($uuidAfter.Substring(0,8))"
    check '文件大小不变（同样的明文与格式）' ($bytesAfter.Length -eq $sizeBefore) "$sizeBefore B"

    # 载荷密文必须整体改变。header_len 在偏移 12..16（小端 u32）
    $hlen = [BitConverter]::ToUInt32($bytesBefore, 12)
    $same = $true
    for ($i = $hlen; $i -lt $sizeBefore; $i++) {
        if ($bytesBefore[$i] -ne $bytesAfter[$i]) { $same = $false; break }
    }
    check '载荷密文已整体改变（这才叫轮换）' (-not $same) "header_len=$hlen"

    # 新密码能解出原明文
    $r1 = Join-Path $work 'r1.txt'
    & $cli decrypt $f -o $r1 --password-env PW_B --yes *> $null
    $okPlain = (Test-Path $r1) -and
        ([Convert]::ToBase64String([IO.File]::ReadAllBytes($r1)) -eq [Convert]::ToBase64String($plainBefore))
    check '新密码能解出与原文逐字节相同的明文' $okPlain ''

    # 旧密码必须失效
    $r2 = Join-Path $work 'r2.txt'
    & $cli decrypt $f -o $r2 --password-env PW_A --yes *> $null 2>&1
    check '旧密码已无法打开' (-not (Test-Path $r2)) ''

    Write-Output ''
    Write-Output '=== 2. 不传新密码时沿用当前密码 ==='
    $g = Join-Path $work 'keep.omy'
    & $cli encrypt $src -o $g --password-env PW_A --kdf-profile mobile --yes *> $null
    $gBefore = [IO.File]::ReadAllBytes($g)
    $hlen2 = [BitConverter]::ToUInt32($gBefore, 12)
    & $cli key reencrypt $g --password-env PW_A --yes *> $null
    check '不传新密码也能执行' ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"
    $gAfter = [IO.File]::ReadAllBytes($g)
    $same2 = $true
    for ($i = $hlen2; $i -lt $gBefore.Length; $i++) {
        if ($gBefore[$i] -ne $gAfter[$i]) { $same2 = $false; break }
    }
    check '载荷仍然被重写了（换了文件密钥）' (-not $same2) ''
    $r3 = Join-Path $work 'r3.txt'
    & $cli decrypt $g -o $r3 --password-env PW_A --yes *> $null
    check '原密码仍然可用' (Test-Path $r3) ''

    Write-Output ''
    Write-Output '=== 3. 轮换保留文件名与格式参数 ==='
    $meta = & $cli info $g --password-env PW_A --json 2>&1 | ConvertFrom-Json
    check '文件名保留' ($meta.filename -eq 'doc.txt') "filename=$($meta.filename)"

    Write-Output ''
    Write-Output '=== 4. 错误处理 ==='
    & $cli key reencrypt $g --password-env PW_C --yes *> $null 2>&1
    check '密码错误时拒绝' ($LASTEXITCODE -ne 0) "exit=$LASTEXITCODE"

    $plain = Join-Path $work 'notomy.txt'
    Set-Content -LiteralPath $plain -Value 'plain' -Encoding UTF8
    & $cli key reencrypt $plain --password-env PW_A --yes *> $null 2>&1
    check '非加密文件时拒绝' ($LASTEXITCODE -ne 0) "exit=$LASTEXITCODE"

    # 轮换失败时原文件必须完好——这是最重要的安全性质
    $before4 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($g))
    & $cli key reencrypt $g --password-env PW_C --yes *> $null 2>&1
    $after4 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($g))
    check '失败后原文件逐字节未改动' ($before4 -eq $after4) ''

    Write-Output ''
    Write-Output '=== 5. JSON 输出契约 ==='
    $h = Join-Path $work 'json.omy'
    & $cli encrypt $src -o $h --password-env PW_A --kdf-profile mobile --yes *> $null
    $j = & $cli key reencrypt $h --password-env PW_A --yes --json 2>&1 | ConvertFrom-Json
    check 'operation 为 reencrypt' ($j.operation -eq 'reencrypt') "operation=$($j.operation)"
    # 这个字段是给增量备份策略看的：改密码不动载荷、轮换动了，
    # 报错会让备份工具做错决定
    check 'payload_rewritten 为 true' ($j.payload_rewritten -eq $true) "payload_rewritten=$($j.payload_rewritten)"
    check 'slots_in_use 为 1' ($j.slots_in_use -eq 1) "slots_in_use=$($j.slots_in_use)"

    # 对照：普通改密码必须报 false
    $j2 = & $cli key change $h --password-env PW_A --new-password-env PW_B --yes --json 2>&1 | ConvertFrom-Json
    check '对照：key change 的 payload_rewritten 为 false' ($j2.payload_rewritten -eq $false) "payload_rewritten=$($j2.payload_rewritten)"

    Write-Output ''
    Write-Output '=== 6. remove 仍拒绝新密码参数（未被本次改动破坏）==='
    $k = Join-Path $work 'rm.omy'
    & $cli encrypt $src -o $k --password-env PW_A --kdf-profile mobile --yes *> $null
    & $cli key remove $k --password-env PW_A --new-password-env PW_B --yes *> $null 2>&1
    check 'remove 拒绝 --new-password-env' ($LASTEXITCODE -ne 0) "exit=$LASTEXITCODE"

    Write-Output ''
    Write-Output '=== 7. 容器（加密文件夹）也能轮换 ==='
    $dir = Join-Path $work 'folder'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Set-Content -LiteralPath (Join-Path $dir 'a.txt') -Value 'inside a' -Encoding UTF8 -NoNewline
    Set-Content -LiteralPath (Join-Path $dir 'b.txt') -Value 'inside b' -Encoding UTF8 -NoNewline
    $c = Join-Path $work 'folder.omy'
    & $cli encrypt $dir -o $c --password-env PW_A --kdf-profile mobile --yes *> $null
    & $cli key reencrypt $c --password-env PW_A --new-password-env PW_B --yes *> $null
    check '容器轮换成功' ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"
    $rest = Join-Path $work 'restored'
    & $cli decrypt $c -o $rest --password-env PW_B --yes *> $null
    # 容器会把原文件夹名作为一层子目录还原（verify-key-slots.ps1 踩过）
    $ra = Join-Path $rest 'folder\a.txt'
    check '容器内文件还原正确' ((Test-Path $ra) -and
        ((Get-Content -LiteralPath $ra -Raw) -eq 'inside a')) ''
    $rest2 = Join-Path $work 'restored2'
    & $cli decrypt $c -o $rest2 --password-env PW_A --yes *> $null 2>&1
    check '容器旧密码已作废' (-not (Test-Path (Join-Path $rest2 'folder\a.txt'))) ''
} finally {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_A, Env:\PW_B, Env:\PW_C -EA SilentlyContinue
}

Write-Output ''
Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
if ($fail -gt 0) { exit 1 }
