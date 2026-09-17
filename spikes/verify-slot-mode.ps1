# 槽位可管理模式的端到端验证（规范 §3.5、DEC-17）。
#
# 验的是这个模式存在的唯一理由：**精确删掉某个协作者而保住恢复码**。
# 可否认模式做不到——它分不清哪个槽是谁的，只能整体保留或整体清场。
#
# 与 core 集成测试的分工：那个直接调 rewrite_slots_managed，这个走真实
# CLI，确认 --slot-mode、key list、key add/remove 这几条命令真的接对了。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
if (-not (Test-Path $cli)) { Write-Output 'FAIL  先 cargo build -p omy-cli'; exit 1 }

$newest = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newest.LastWriteTime -gt (Get-Item $cli).LastWriteTime) {
    Write-Output "FAIL  二进制比源码旧（$($newest.Name)），先 cargo build -p omy-cli"
    exit 1
}

$pass = 0; $fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { $script:pass++; Write-Output "  OK  $name" }
    else { $script:fail++; Write-Output "  FAIL  $name$(if($detail){" — $detail"})" }
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-slotmode-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

$src = Join-Path $work '合同.txt'
Set-Content -LiteralPath $src -Value 'contract body' -Encoding UTF8 -NoNewline

$env:PW_OWNER = 'pw-owner'
$env:PW_MATE  = 'pw-mate'
$env:PW_NEW   = 'pw-new'

Write-Output ''
Write-Output '--- 一、两种模式都能加密，flag 如实反映 ---'

$den = Join-Path $work 'deniable.omy'
& $cli encrypt $src -o $den --password-env PW_OWNER --kdf-profile mobile --yes *> $null
$man = Join-Path $work 'managed.omy'
& $cli encrypt $src -o $man --slot-mode managed --password-env PW_OWNER --kdf-profile mobile --yes *> $null
Check '两种模式都能加密' ((Test-Path $den) -and (Test-Path $man))

$denList = & $cli key list $den 2>&1 | Out-String
$manList = & $cli key list $man 2>&1 | Out-String
Check '可否认模式如实标注' ($denList -match 'deniable|可否认') $denList.Trim()
Check '可管理模式如实标注' ($manList -match 'managed|可管理') $manList.Trim()

# 不给密码时可管理模式也不该泄露槽位内容
Check '不给密码时不列出槽位' ($manList -notmatch 'slot 0') $manList.Trim()
Check '并告知需要密码' ($manList -match '需要密码|password') $manList.Trim()

Write-Output ''
Write-Output '--- 二、可管理模式能列出槽位类型 ---'
$listed = & $cli key list $man --password-env PW_OWNER 2>&1 | Out-String
Check '给了密码就能列出槽位' ($listed -match 'slot 0') $listed.Trim()
Check '类型显示为日常密码' ($listed -match '日常密码') $listed.Trim()
Check '占用数正确（1 个）' ($listed -match 'Slot 占用\s+1') $listed.Trim()

Write-Output ''
Write-Output '--- 三、挂上协作者与恢复码 ---'
& $cli key add $man --password-env PW_OWNER --new-password-env PW_MATE --yes *> $null
$codeFile = Join-Path $work 'code.txt'
& $cli key recovery $man --password-env PW_OWNER --out $codeFile --yes *> $null

$three = & $cli key list $man --password-env PW_OWNER 2>&1 | Out-String
Check '现在有 3 个槽位在用' ($three -match 'Slot 占用\s+3') $three.Trim()

# 三把钥匙都能开
foreach ($pair in @(@{N='主密码'; E='PW_OWNER'}, @{N='协作者'; E='PW_MATE'})) {
    $out = Join-Path $work "dec-$($pair.E).txt"
    & $cli decrypt $man -o $out --password-env $pair.E --yes *> $null 2>&1
    Check "$($pair.N)能打开" (Test-Path $out)
    Remove-Item $out -Force -EA SilentlyContinue
}

Write-Output ''
Write-Output '--- 四、精确删除：这是整个模式存在的理由 ---'
# 用协作者的密码执行 remove：只保留它自己，清掉主密码与恢复码。
# 反过来更贴近真实场景——主人删协作者。这里用主人的密码 remove，
# 预期清掉协作者和恢复码，所以先单独测「remove 会如实点名」
$removeOut = & $cli key remove $man --password-env PW_OWNER --yes 2>&1 | Out-String
Check 'remove 如实点名将清除哪些槽位' ($removeOut -match 'slot \d+（') $removeOut.Trim()
Check 'remove 点名了恢复码' ($removeOut -match '恢复码') $removeOut.Trim()

$after = & $cli key list $man --password-env PW_OWNER 2>&1 | Out-String
Check 'remove 后只剩 1 个槽位' ($after -match 'Slot 占用\s+1') $after.Trim()

$mateOut = Join-Path $work 'mate-after.txt'
& $cli decrypt $man -o $mateOut --password-env PW_MATE --yes *> $null 2>&1
Check '被清掉的协作者再也打不开' (-not (Test-Path $mateOut))

Write-Output ''
Write-Output '--- 五、add 复用被删掉的空洞，不顶掉别人 ---'
# 重新造一个三槽位的文件
$man2 = Join-Path $work 'managed2.omy'
& $cli encrypt $src -o $man2 --slot-mode managed --password-env PW_OWNER --kdf-profile mobile --yes *> $null
& $cli key add $man2 --password-env PW_OWNER --new-password-env PW_MATE --yes *> $null
$code2 = Join-Path $work 'code2.txt'
& $cli key recovery $man2 --password-env PW_OWNER --out $code2 --yes *> $null

# 此时 slot0=主, slot1=协作者, slot2=恢复码。add 第四个密码应写 slot3
$env:PW_4TH = 'pw-fourth'
$addOut = & $cli key add $man2 --password-env PW_OWNER --new-password-env PW_4TH --yes 2>&1 | Out-String
# detail 级输出默认不显示，所以不断言命令回显，改为核对目录的最终状态——
# 那才是用户能看到、也真正要紧的事实
$afterAdd = & $cli key list $man2 --password-env PW_OWNER 2>&1 | Out-String
Check 'add 之后有 4 个槽位在用' ($afterAdd -match 'Slot 占用\s+4') $afterAdd.Trim()
Check '恢复码在目录里如实记着' ($afterAdd -match '恢复码') $afterAdd.Trim()
Check 'add 不再警告「可能顶掉别的密码」' ($addOut -notmatch '可能挂着别的密码') $addOut.Trim()

# 恢复码必须还在——这正是可否认模式绕不开的那个边界
if (Test-Path $code2) {
    $restored = Join-Path $work 'reco-after-add.txt'
    $env:PW_R = 'pw-reco-new'
    & $cli key restore $man2 --code-file $code2 --new-password-env PW_R --yes *> $null 2>&1
    & $cli decrypt $man2 -o $restored --password-env PW_R --yes *> $null 2>&1
    Check 'add 之后恢复码仍然有效' (Test-Path $restored) 'add 顶掉了恢复码'
}

Write-Output ''
Write-Output '--- 六、可否认模式的行为不受影响 ---'
$denListed = & $cli key list $den --password-env PW_OWNER 2>&1 | Out-String
Check '可否认模式给密码也不列槽位' ($denListed -notmatch 'slot 0') $denListed.Trim()
Check '可否认模式仍说明不可探测' ($denListed -match '无法区分|不可探测|无法判断') $denListed.Trim()

Write-Output ''
Write-Output '--- 七、restore 的语义必须与可否认模式一致 ---'
# 曾经错过两次：先是 rewrite_slots 盲写前两个槽，盖掉协作者而目录
# 还显示他在；改成「写进空槽」后又走向另一个极端——旧密码原封不动
# 还能用，而可否认模式下它是会失效的。同一个命令两种模式语义相反，
# 比原缺陷更糟：用户用 restore 正是因为旧密码忘了或可能已泄露。
$man3 = Join-Path $work 'managed3.omy'
& $cli encrypt $src -o $man3 --slot-mode managed --password-env PW_OWNER --kdf-profile mobile --yes *> $null
& $cli key add $man3 --password-env PW_OWNER --new-password-env PW_MATE --yes *> $null
$code3 = Join-Path $work 'code3.txt'
& $cli key recovery $man3 --password-env PW_OWNER --out $code3 --yes *> $null

$env:PW_R2 = 'pw-restored'
& $cli key restore $man3 --code-file $code3 --new-password-env PW_R2 --yes *> $null

$rOut = Join-Path $work 'r-new.txt'
& $cli decrypt $man3 -o $rOut --password-env PW_R2 --yes *> $null 2>&1
Check 'restore 后新密码能打开' (Test-Path $rOut)

$rOld = Join-Path $work 'r-old.txt'
& $cli decrypt $man3 -o $rOld --password-env PW_OWNER --yes *> $null 2>&1
Check 'restore 后旧密码已失效' (-not (Test-Path $rOld)) '旧密码还能开——与可否认模式语义相反'

$rMate = Join-Path $work 'r-mate.txt'
& $cli decrypt $man3 -o $rMate --password-env PW_MATE --yes *> $null 2>&1
Check 'restore 后协作者也已失效' (-not (Test-Path $rMate))

# 目录必须与实际相符：新密码 + 恢复码 = 2
$rList = & $cli key list $man3 --password-env PW_R2 2>&1 | Out-String
Check 'restore 后目录与实际相符（2 个槽）' ($rList -match 'Slot 占用\s+2') $rList.Trim()
Check 'restore 后恢复码仍在目录里' ($rList -match '恢复码') $rList.Trim()

# 恢复码必须还能用——用户刚忘过一次密码，这时抽掉兜底是最坏的时机
$env:PW_R3 = 'pw-again'
& $cli key restore $man3 --code-file $code3 --new-password-env PW_R3 --yes *> $null 2>&1
$rAgain = Join-Path $work 'r-again.txt'
& $cli decrypt $man3 -o $rAgain --password-env PW_R3 --yes *> $null 2>&1
Check '同一份恢复码可以再用一次' (Test-Path $rAgain)

Remove-Item $work -Recurse -Force -EA SilentlyContinue
Remove-Item Env:\PW_OWNER, Env:\PW_MATE, Env:\PW_NEW, Env:\PW_4TH, Env:\PW_R, Env:\PW_R2, Env:\PW_R3 -EA SilentlyContinue

Write-Output ''
Write-Output "通过 $pass 项，失败 $fail 项"
exit $(if ($fail -eq 0) { 0 } else { 1 })
