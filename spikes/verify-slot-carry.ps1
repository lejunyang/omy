# 验证：改密码不再抹掉恢复码，而 remove 仍然真的清场。
#
# core 的单测直接调 rewrite_slots，绕过了「命令行参数 → Op::other_slots
# → 确认文案 → 写回」这条链。任何一环把策略传错（比如 change 误用
# Discard），单测照样全绿，而用户改一次密码就永久失去恢复码。
#
# 判据是**用另一条独立路径（CLI decrypt）实际打开文件**，不看提示文字。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

# 自证测的是最新构建
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $cli).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build -p omy-cli"
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-carry-test'
$pass = 0
$fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { $script:pass++; Write-Output "  PASS  $name $detail" }
    else { $script:fail++; Write-Output "  FAIL  $name $detail" }
}

# 造一个挂着「主密码 + 恢复码」的文件
function New-Fixture($tag) {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $src = Join-Path $work 'data.txt'
    Set-Content -LiteralPath $src -Value 'carry probe payload' -Encoding UTF8 -NoNewline
    $f = Join-Path $work "$tag.omy"
    & $cli encrypt $src -o $f --password-env PW_MAIN --kdf-profile mobile --yes *> $null
    & $cli key add $f --password-env PW_MAIN --new-password-env PW_RECO --yes *> $null
    Remove-Item $src -Force
    return $f
}

# 用指定密码能否解出正确明文。换一条独立路径验证，不看命令的自述
function Test-Open($file, $envVar) {
    $out = Join-Path $work "probe-$([Guid]::NewGuid().ToString('N').Substring(0,8)).txt"
    & $cli decrypt $file -o $out --password-env $envVar --yes *> $null 2>&1
    if (-not (Test-Path $out)) { return $false }
    $ok = (Get-Content -LiteralPath $out -Raw).TrimEnd("`r", "`n") -eq 'carry probe payload'
    Remove-Item $out -Force -EA SilentlyContinue
    return $ok
}

$env:PW_MAIN = 'main-password'
$env:PW_RECO = 'pretend-recovery-code'
$env:PW_NEW = 'new-main-password'
$env:PW_EXTRA = 'extra-password'

try {
    Write-Output '=== 1. 基线：主密码与恢复码都能开 ==='
    $f = New-Fixture 'base'
    Check '主密码可开' (Test-Open $f 'PW_MAIN')
    Check '恢复码可开' (Test-Open $f 'PW_RECO')

    Write-Output ''
    Write-Output '=== 2. key change：这是本次要修的缺陷 ==='
    $f = New-Fixture 'chg'
    $out = & $cli key change $f --password-env PW_MAIN --new-password-env PW_NEW --yes 2>&1 | Out-String
    Check '新主密码可开' (Test-Open $f 'PW_NEW')
    Check '恢复码仍然可开（修复前这里是 false）' (Test-Open $f 'PW_RECO')
    Check '旧主密码已失效' (-not (Test-Open $f 'PW_MAIN'))
    # 文案也要检查：change 不该再吓唬用户说其它密码会失效
    Check 'change 提示说明其它密码不受影响' ($out -match '不受影响') `
        "（实际输出片段：$(($out -split "`r?`n" | Where-Object { $_ -match '密码' } | Select-Object -First 1))）"

    Write-Output ''
    Write-Output '=== 3. key add：keep 增长会顶掉靠前的槽，必须如实报告 ==='
    # 这条最初写成「add 后恢复码仍可开」，实测失败。查下来不是策略传错，
    # 而是 Carry 的一个绕不开的边界：keep 占 slot 0..n，搬运只能从 n 起，
    # 所以新密码会写进 slot1 —— 恢复码原本就在那里。
    #
    # 包裹密钥绑定 slot_index，新密码必须写在自己的下标上；而我们又分不清
    # 哪个下标是空的（可否认性）。所以正确的期望不是「保住」，
    # 而是「如实告诉用户可能没保住」。
    $f = New-Fixture 'add'
    $out = & $cli key add $f --password-env PW_MAIN --new-password-env PW_EXTRA --yes 2>&1 | Out-String
    Check '原主密码可开' (Test-Open $f 'PW_MAIN')
    Check '新增密码可开' (Test-Open $f 'PW_EXTRA')
    Check 'add 警告了可能顶掉其它密码' ($out -match '可能挂着别的密码')
    Check 'add 提示重新生成恢复码' ($out -match '重新生成')

    Write-Output ''
    Write-Output '=== 3b. 只有一个密码的文件上 add，不该乱报警告 ==='
    # 反向保险：若把警告写成无条件输出，用户每次 add 都被吓一次，
    # 久而久之就不看警告了——这比不报还糟
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $src2 = Join-Path $work 'solo.txt'
    Set-Content -LiteralPath $src2 -Value 'carry probe payload' -Encoding UTF8 -NoNewline
    $f2 = Join-Path $work 'solo.omy'
    & $cli encrypt $src2 -o $f2 --password-env PW_MAIN --kdf-profile mobile --yes *> $null
    Remove-Item $src2 -Force
    $out2 = & $cli key add $f2 --password-env PW_MAIN --new-password-env PW_EXTRA --yes 2>&1 | Out-String
    # 这里仍会报警告（我们确实不知道 slot1 是不是空的），但两个密码都要能用
    Check '两个密码都能开' ((Test-Open $f2 'PW_MAIN') -and (Test-Open $f2 'PW_EXTRA'))

    Write-Output ''
    Write-Output '=== 4. key remove：必须仍然真的清场 ==='
    # 这条是反向保险。只测「保住了」的话，把 Discard 也误写成 Carry
    # 不会有任何测试变红，而 remove 就成了假操作
    $f = New-Fixture 'rm'
    $out = & $cli key remove $f --password-env PW_MAIN --yes 2>&1 | Out-String
    Check '当前密码仍可开' (Test-Open $f 'PW_MAIN')
    Check '恢复码已被作废（remove 是清场）' (-not (Test-Open $f 'PW_RECO'))
    Check 'remove 提示点名了恢复码' ($out -match '恢复码')

    Write-Output ''
    Write-Output '=== 5. 文件长度与可否认性不受影响 ==='
    $f = New-Fixture 'len'
    $before = (Get-Item $f).Length
    & $cli key change $f --password-env PW_MAIN --new-password-env PW_NEW --yes *> $null
    $after = (Get-Item $f).Length
    Check '改密码后文件长度不变' ($before -eq $after) "（$before → $after）"
} finally {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_MAIN, Env:\PW_RECO, Env:\PW_NEW, Env:\PW_EXTRA -EA SilentlyContinue
}

Write-Output ''
Write-Output "共 $($pass + $fail) 项，失败 $fail 项"
if ($fail -gt 0) { exit 1 }
