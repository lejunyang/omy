# omy key add / remove / change 的端到端实测。
#
# 为什么必须端到端：单测直接调 rewrite_slots，绕过了「命令行参数 → 密码
# 读取 → keep 集合构造 → 写回」这条链。上一轮回收站的教训就是后端分支
# 写好了但 UI 里根本没有那个入口，单测全绿而功能不可用。
#
# 核心断言是「该开的能开、该不能开的真的打不开」，而不是「命令退出码为 0」。

$ErrorActionPreference = 'Continue'
Set-Location 'E:\Projects\omy'
$omy = 'E:\Projects\omy\target\debug\omy.exe'

if (-not (Test-Path $omy)) { Write-Output "FAIL 找不到 $omy"; exit 1 }

# 自证测的是最新构建
$newest = Get-ChildItem 'crates\omy-cli\src', 'crates\omy-core\src' -Recurse -Filter *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ((Get-Item $omy).LastWriteTime -lt $newest.LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newest.Name)），先 cargo build -p omy-cli"
    exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newest.Name)）"

$work = Join-Path $env:TEMP 'omy-key-e2e'
Remove-Item -Recurse -Force $work -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

$env:PW_A = 'alpha-pass-1'
$env:PW_B = 'bravo-pass-2'
$env:PW_C = 'charlie-pass-3'

$pass = 0
$fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { Write-Output "  PASS  $name  $detail"; $script:pass++ }
    else { Write-Output "  FAIL  $name  $detail"; $script:fail++ }
}

# 能否用某个密码解开：解密到临时文件并比对内容
function CanOpen($enc, $envVar, $expect) {
    $tmp = Join-Path $work ("probe-" + [guid]::NewGuid().ToString('N') + ".txt")
    & $omy decrypt $enc -o $tmp --password-env $envVar --yes *> $null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $tmp)) { return $false }
    $got = Get-Content -LiteralPath $tmp -Raw
    Remove-Item $tmp -Force -EA SilentlyContinue
    return ($got.TrimEnd("`r", "`n") -eq $expect)
}

$body = 'key-slot-e2e-payload'
$src = Join-Path $work 'doc.txt'
Set-Content -LiteralPath $src -Value $body -Encoding UTF8 -NoNewline

Write-Output ''
Write-Output '=== 1. key add：原密码与新密码都要能开 ==='
$f1 = Join-Path $work 'add.omy'
& $omy encrypt $src -o $f1 --password-env PW_A --kdf-profile mobile --yes *> $null
Check '加密产物存在' (Test-Path $f1)
$sizeBefore = (Get-Item $f1).Length
$hashPayloadBefore = (Get-FileHash $f1 -Algorithm SHA256).Hash

& $omy key add $f1 --password-env PW_A --new-password-env PW_B --yes *> $null
Check 'key add 退出码为 0' ($LASTEXITCODE -eq 0)
Check 'add 后原密码仍可解密' (CanOpen $f1 'PW_A' $body)
Check 'add 后新密码可解密' (CanOpen $f1 'PW_B' $body)
Check 'add 后无关密码打不开' (-not (CanOpen $f1 'PW_C' $body))
Check '文件大小不变（未重写载荷）' ((Get-Item $f1).Length -eq $sizeBefore) `
    "$sizeBefore -> $((Get-Item $f1).Length)"
Check '文件内容确实变了（slot 区重写）' `
    ((Get-FileHash $f1 -Algorithm SHA256).Hash -ne $hashPayloadBefore)

Write-Output ''
Write-Output '=== 2. 载荷与 file_uuid 逐字节未变 ==='
# 用 info --json 取 uuid；载荷部分用字节比对（头部 512 字节之后）
$f2 = Join-Path $work 'uuid.omy'
& $omy encrypt $src -o $f2 --password-env PW_A --kdf-profile mobile --yes *> $null
$before = [System.IO.File]::ReadAllBytes($f2)
& $omy key add $f2 --password-env PW_A --new-password-env PW_B --yes *> $null
$after = [System.IO.File]::ReadAllBytes($f2)
Check '长度一致' ($before.Length -eq $after.Length)
# 固定头 96 字节：magic..tlv_len，其中 file_uuid 在偏移 16..32
$uuidSame = $true
for ($i = 16; $i -lt 32; $i++) { if ($before[$i] -ne $after[$i]) { $uuidSame = $false; break } }
Check 'file_uuid 未变' $uuidSame
# slot 区 = 96..480，必须变
$slotChanged = $false
for ($i = 96; $i -lt 480; $i++) { if ($before[$i] -ne $after[$i]) { $slotChanged = $true; break } }
Check 'slot 区已变' $slotChanged
# 载荷：从 header_len 之后。header_len 在偏移 12..16（小端 u32）
$hlen = [BitConverter]::ToUInt32($before, 12)
$payloadSame = $true
for ($i = $hlen; $i -lt $before.Length; $i++) {
    if ($before[$i] -ne $after[$i]) { $payloadSame = $false; break }
}
Check "载荷逐字节相同（header_len=$hlen）" $payloadSame

Write-Output ''
Write-Output '=== 3. key change：旧密码作废，新密码可用 ==='
$f3 = Join-Path $work 'change.omy'
& $omy encrypt $src -o $f3 --password-env PW_A --kdf-profile mobile --yes *> $null
& $omy key change $f3 --password-env PW_A --new-password-env PW_B --yes *> $null
Check 'key change 退出码为 0' ($LASTEXITCODE -eq 0)
Check 'change 后新密码可解密' (CanOpen $f3 'PW_B' $body)
Check 'change 后旧密码打不开' (-not (CanOpen $f3 'PW_A' $body))

Write-Output ''
Write-Output '=== 4. key remove：只留当前密码，其它作废 ==='
$f4 = Join-Path $work 'remove.omy'
& $omy encrypt $src -o $f4 --password-env PW_A --kdf-profile mobile --yes *> $null
& $omy key add $f4 --password-env PW_A --new-password-env PW_B --yes *> $null
Check 'remove 前两个密码都可用' ((CanOpen $f4 'PW_A' $body) -and (CanOpen $f4 'PW_B' $body))
# 用 B 解锁并 remove：应当只剩 B
& $omy key remove $f4 --password-env PW_B --yes *> $null
Check 'key remove 退出码为 0' ($LASTEXITCODE -eq 0)
Check 'remove 后解锁用的密码仍可用' (CanOpen $f4 'PW_B' $body)
Check 'remove 后另一个密码已作废' (-not (CanOpen $f4 'PW_A' $body))

Write-Output ''
Write-Output '=== 5. 错误处理 ==='
$f5 = Join-Path $work 'err.omy'
& $omy encrypt $src -o $f5 --password-env PW_A --kdf-profile mobile --yes *> $null
$snapshot = (Get-FileHash $f5 -Algorithm SHA256).Hash

& $omy key add $f5 --password-env PW_C --new-password-env PW_B --yes *> $null 2>&1
Check '密码错误时退出码非 0' ($LASTEXITCODE -ne 0)
Check '密码错误时文件未被改动' `
    ((Get-FileHash $f5 -Algorithm SHA256).Hash -eq $snapshot)

$out = & $omy key change $f5 --password-env PW_A --new-password-env PW_A --yes 2>&1 | Out-String
Check '新旧密码相同时报错' ($LASTEXITCODE -ne 0) $out.Trim().Split("`n")[0]
Check '新旧密码相同时文件未改动' `
    ((Get-FileHash $f5 -Algorithm SHA256).Hash -eq $snapshot)

Write-Output ''
Write-Output '=== 6. 不再出现「尚未实现」 ==='
foreach ($sub in @('add', 'remove', 'change')) {
    $o = & $omy key $sub $f5 --password-env PW_A --new-password-env PW_B --yes 2>&1 | Out-String
    Check "key $sub 不报未实现" (-not ($o -match '尚未实现|not implemented'))
    # 每次操作后把密码改回 A，便于下一轮复用
    & $omy key change $f5 --password-env PW_B --new-password-env PW_A --yes *> $null
}

Write-Output ''
Write-Output '=== 7. key list 仍如实说明不可探测 ==='
$o = & $omy key list $f5 2>&1 | Out-String
Check 'list 提到 8 个 slot' ($o -match '8')
Check 'list 说明不可探测' ($o -match '不可探测|无法区分')
$j = & $omy --json key list $f5 2>&1 | Out-String
$ok = $true
try { $p = $j | ConvertFrom-Json; $ok = ($null -eq $p.slot_used) } catch { $ok = $false }
Check 'JSON 里 slot_used 恒为 null' $ok

Write-Output ''
Write-Output '=== 8. 容器（加密文件夹）也能改密码 ==='
$dir = Join-Path $work 'folder'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Set-Content -LiteralPath (Join-Path $dir 'a.txt') -Value 'inner-a' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath (Join-Path $dir 'b.txt') -Value 'inner-b' -Encoding UTF8 -NoNewline
$fc = Join-Path $work 'folder.omy'
& $omy encrypt $dir -o $fc --password-env PW_A --kdf-profile mobile --yes *> $null
Check '容器加密成功' (Test-Path $fc)
& $omy key change $fc --password-env PW_A --new-password-env PW_B --yes *> $null
Check '容器 key change 成功' ($LASTEXITCODE -eq 0)
$outdir = Join-Path $work 'restored'
& $omy decrypt $fc -o $outdir --password-env PW_B --yes *> $null
# 容器会把原文件夹名作为一层子目录还原，所以是 restored\folder\a.txt。
# 一开始按 restored\a.txt 断言，结果把「路径写错」显示成了产品缺陷
$inner = Join-Path (Join-Path $outdir 'folder') 'a.txt'
Check '容器可用新密码解出' (Test-Path $inner) $inner
if (Test-Path $inner) {
    $c = Get-Content -LiteralPath $inner -Raw
    Check '容器内文件内容正确' ($c.TrimEnd("`r", "`n") -eq 'inner-a')
}
# 旧密码必须已作废——容器和单文件走同一条 slot 路径，但值得单独证一次
$oldout = Join-Path $work 'restored-old'
& $omy decrypt $fc -o $oldout --password-env PW_A --yes *> $null 2>&1
Check '容器旧密码已作废' ($LASTEXITCODE -ne 0)

Remove-Item -Recurse -Force $work -EA SilentlyContinue
Remove-Item Env:\PW_A, Env:\PW_B, Env:\PW_C -EA SilentlyContinue

Write-Output ''
Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
if ($fail -gt 0) { exit 1 }
exit 0
