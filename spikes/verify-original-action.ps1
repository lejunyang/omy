# 实测 CLI 的 --original：三种取值都要真的生效，配置回落也要生效。
#
# 为什么必须端到端跑：单测只覆盖枚举解析，覆盖不到
# 「参数 → 配置回落 → 真正处置原件」这条链。配置项此前定义了却
# 从没被读过，正是这种缺口——单测全绿，功能不存在。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$omy = Join-Path $root 'target\debug\omy.exe'

$newest = Get-ChildItem (Join-Path $root 'crates\omy-cli\src') -Recurse -Filter *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ((Get-Item $omy).LastWriteTime -lt $newest.LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newest.Name)）"; exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newest.Name)）"

$work = Join-Path $env:TEMP 'omy-original-test'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$env:OMY_TEST_PW = 'orig-pw-123'

$pass = 0; $fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { Write-Output "  PASS  $name  $detail"; $script:pass++ }
    else { Write-Output "  FAIL  $name  $detail"; $script:fail++ }
}

function New-Victim($name) {
    $p = Join-Path $work $name
    Set-Content -LiteralPath $p -Value "content of $name" -Encoding UTF8 -NoNewline
    return $p
}

function Run-Omy($argList) {
    $o = Join-Path $work 'o.txt'; $e = Join-Path $work 'e.txt'
    $p = Start-Process -FilePath $omy -NoNewWindow -Wait -PassThru `
        -ArgumentList $argList -RedirectStandardOutput $o -RedirectStandardError $e
    return @{ Code = $p.ExitCode; Err = (Get-Content $e -Raw -EA SilentlyContinue) }
}

Write-Output ''
Write-Output '=== 1. --original keep：原件必须还在 ==='
$v = New-Victim 'keep-me.txt'
$r = Run-Omy @('encrypt', $v, '-o', "$v.omy", '--password-env', 'OMY_TEST_PW', '--kdf-profile', 'mobile', '--original', 'keep', '--yes')
Check '退出码 0' ($r.Code -eq 0) "code=$($r.Code)"
Check '原件保留' (Test-Path $v)

Write-Output ''
Write-Output '=== 2. --original delete：原件必须消失，且不在回收站 ==='
$v2 = New-Victim 'delete-me.txt'
$r2 = Run-Omy @('encrypt', $v2, '-o', "$v2.omy", '--password-env', 'OMY_TEST_PW', '--kdf-profile', 'mobile', '--original', 'delete', '--yes')
Check '退出码 0' ($r2.Code -eq 0) "code=$($r2.Code)"
Check '原件已删除' (-not (Test-Path $v2))

Write-Output ''
Write-Output '=== 3. --original trash：原件消失且能在回收站找到 ==='
$v3 = New-Victim 'trash-me.txt'
$r3 = Run-Omy @('encrypt', $v3, '-o', "$v3.omy", '--password-env', 'OMY_TEST_PW', '--kdf-profile', 'mobile', '--original', 'trash', '--yes')
Check '退出码 0' ($r3.Code -eq 0) "code=$($r3.Code)"
Check '原件已从原位置移走' (-not (Test-Path $v3))
$shell = New-Object -ComObject Shell.Application
$inBin = $false
foreach ($it in @($shell.Namespace(10).Items())) {
    if ($it.Name -eq 'trash-me.txt' -or $it.Name -eq 'trash-me') { $inBin = $true; break }
}
Check '回收站里找得到（可还原，不是永久删除）' $inBin

Write-Output ''
Write-Output '=== 4. 配置回落：不传 --original 时读 defaults.original_action ==='
# 这一项是本次改造的核心：配置项此前从未被读过
$cfgDir = Join-Path $work 'cfg'
New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
@'
[defaults]
original_action = "delete"
'@ | Set-Content -Path (Join-Path $cfgDir 'config.toml') -Encoding utf8
$cfgFile = Join-Path $cfgDir 'config.toml'
$v4 = New-Victim 'from-config.txt'
# --config 是全局参数，必须放在子命令**之前**
$r4 = Run-Omy @('--config', $cfgFile, 'encrypt', $v4, '-o', "$v4.omy", '--password-env', 'OMY_TEST_PW', '--kdf-profile', 'mobile', '--yes')
Check '退出码 0' ($r4.Code -eq 0) "code=$($r4.Code) err=$($r4.Err)"
Check '配置里的 delete 真的生效了（原件消失）' (-not (Test-Path $v4)) `
    '配置项此前定义了却从没被读过，这一项就是守它的'

Write-Output ''
Write-Output '=== 5. 旧参数必须已经不存在（避免两套并存） ==='
$r5 = Run-Omy @('encrypt', '--help')
$hasOld = $r5.Err -match 'delete-original|keep-original'
Check '--delete-original / --keep-original 已移除' (-not $hasOld)

Remove-Item -Recurse -Force $work -EA SilentlyContinue
Remove-Item Env:\OMY_TEST_PW -EA SilentlyContinue

# 清掉本次扔进回收站的测试文件，不给用户留垃圾
$n = 0
foreach ($it in @($shell.Namespace(10).Items())) {
    if ($it.Name -eq 'trash-me.txt' -or $it.Name -eq 'trash-me') {
        Remove-Item -LiteralPath $it.Path -Force -EA SilentlyContinue; $n++
    }
}
Write-Output ''
Write-Output "已清理回收站测试文件 $n 个"
Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
if ($fail -gt 0) { exit 1 }
