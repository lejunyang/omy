# 验证恢复码的完整链路：生成 → 抄写 → 用它重设密码。
#
# core 的 11 项单测只覆盖编解码，绕过了「命令行 → 挂进 slot → 写回磁盘 →
# 换台命令再读回来」这条链。任何一环出错（比如挂的时候没保留主密码、
# 或者 restore 把恢复码本身抽掉了），单测照样全绿。
#
# 判据全部是「某个密码能不能真的解出正确明文」，不看命令的自述。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $cli).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)）"
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-recovery-test'
$pass = 0
$fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { $script:pass++; Write-Output "  PASS  $name $detail" }
    else { $script:fail++; Write-Output "  FAIL  $name $detail" }
}

function New-Fixture {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $src = Join-Path $work 'data.txt'
    Set-Content -LiteralPath $src -Value 'recovery e2e payload' -Encoding UTF8 -NoNewline
    $f = Join-Path $work 'secret.omy'
    & $cli encrypt $src -o $f --password-env PW_MAIN --kdf-profile mobile --yes *> $null
    Remove-Item $src -Force
    return $f
}

function Test-Open($file, $envVar) {
    $out = Join-Path $work "o-$([Guid]::NewGuid().ToString('N').Substring(0,8)).txt"
    & $cli decrypt $file -o $out --password-env $envVar --yes *> $null 2>&1
    if (-not (Test-Path $out)) { return $false }
    $ok = (Get-Content -LiteralPath $out -Raw).TrimEnd("`r", "`n") -eq 'recovery e2e payload'
    Remove-Item $out -Force -EA SilentlyContinue
    return $ok
}

$env:PW_MAIN = 'daily-password'
$env:PW_NEW = 'brand-new-password'

try {
    Write-Output '=== 1. 生成恢复码 ==='
    $f = New-Fixture
    $codeFile = Join-Path $work 'code.txt'
    $out = & $cli key recovery $f --password-env PW_MAIN --out $codeFile 2>&1 | Out-String

    Check '恢复码文件已生成' (Test-Path $codeFile)
    $phrase = if (Test-Path $codeFile) { (Get-Content -LiteralPath $codeFile -Raw).Trim() } else { '' }
    $words = $phrase -split '\s+' | Where-Object { $_ }
    Check '恢复码是 26 个词' ($words.Count -eq 26) "（实际 $($words.Count)）"
    Check '主密码仍然可用（不能被换掉）' (Test-Open $f 'PW_MAIN')
    # 三条警告缺一不可：它们是这个功能诚实与否的全部体现
    Check '警告了只显示一次' ($out -match '只显示这一次')
    Check '警告了它是强度下限' ($out -match '强度下限')
    Check '说明了不参与目录扫描' ($out -match '不会自动显形')

    Write-Output ''
    Write-Output '=== 2. --json 不得泄露恢复码 ==='
    # 这条是安全断言：--json 常被重定向进文件或管道进日志，
    # 把万能钥匙写进那里等于白设恢复码
    $f2 = New-Fixture
    $j = & $cli key recovery $f2 --password-env PW_MAIN --json 2>&1 | Out-String
    $leaked = $false
    foreach ($w in $words) {
        # 随便挑几个词看在不在 JSON 里。只要恢复码进了 JSON，
        # 它的词必然出现
        if ($j -match "\b$w\b") { $leaked = $true; break }
    }
    Check 'JSON 输出里没有恢复码的词' (-not $leaked)
    Check 'JSON 里报了词数' ($j -match '"words"\s*:\s*26')

    Write-Output ''
    Write-Output '=== 3. 用恢复码重设密码（核心流程）==='
    $f = New-Fixture
    & $cli key recovery $f --password-env PW_MAIN --out $codeFile *> $null
    $phrase = (Get-Content -LiteralPath $codeFile -Raw).Trim()

    # 模拟用户从纸上抄回来：大小写混乱、空格多余
    $messy = (($phrase -split '\s+') | ForEach-Object -Begin { $i = 0 } -Process {
        $i++
        if ($i % 3 -eq 0) { $_.ToUpper() } else { $_ }
    }) -join '   '
    $messyFile = Join-Path $work 'messy.txt'
    Set-Content -LiteralPath $messyFile -Value $messy -Encoding UTF8 -NoNewline

    $out = & $cli key restore $f --code-file $messyFile --new-password-env PW_NEW 2>&1 | Out-String
    Check '抄写噪声被容忍' ($out -notmatch '不在词表')
    Check '新密码可打开' (Test-Open $f 'PW_NEW')
    Check '旧主密码已失效' (-not (Test-Open $f 'PW_MAIN'))
    # 刚忘过一次密码的人，最不该在这时被抽掉唯一的兜底
    Check 'restore 声明恢复码仍然有效' ($out -match '仍然有效')

    Write-Output ''
    Write-Output '=== 4. restore 之后恢复码必须还能再用一次 ==='
    # 上一条只看命令怎么说，这条实际再走一遍。
    #
    # 用**全新的 fixture** 而不是接着上一步的文件：上一步已经把密码换成
    # PW_NEW 了，接着用会让「第二次 restore 成功」与「第一次的残留状态」
    # 纠缠在一起。变异测试抓到过这个——把 restore 的 keep 改成不含恢复码
    # 之后，这条竟然还是 PASS，因为它测的其实是上一步留下的结果。
    $env:PW_SECOND = 'second-new-password'
    $f4 = New-Fixture
    $code4 = Join-Path $work 'code4.txt'
    & $cli key recovery $f4 --password-env PW_MAIN --out $code4 *> $null
    # 第一次 restore：把密码从 PW_MAIN 换成 PW_NEW
    & $cli key restore $f4 --code-file $code4 --new-password-env PW_NEW *> $null
    Check '第一次 restore 生效' (Test-Open $f4 'PW_NEW')
    # 第二次 restore：这一步只有在恢复码被保留时才可能成功
    $out2 = & $cli key restore $f4 --code-file $code4 --new-password-env PW_SECOND 2>&1 | Out-String
    Check '同一份恢复码可再次使用（restore 必须保留它）' (Test-Open $f4 'PW_SECOND') `
        "（$(($out2 -split "`r?`n" | Where-Object { $_ -match '打不开|error' } | Select-Object -First 1))）"

    Write-Output ''
    Write-Output '=== 5. 抄错一个词：必须指出第几个，而不是笼统报错 ==='
    $f = New-Fixture
    & $cli key recovery $f --password-env PW_MAIN --out $codeFile *> $null
    $ws = (Get-Content -LiteralPath $codeFile -Raw).Trim() -split '\s+'
    $original = $ws[6]
    $ws[6] = $ws[6].Substring(0, $ws[6].Length - 1)  # 漏抄最后一个字母
    $typoFile = Join-Path $work 'typo.txt'
    Set-Content -LiteralPath $typoFile -Value ($ws -join ' ') -Encoding UTF8 -NoNewline
    $out = & $cli key restore $f --code-file $typoFile --new-password-env PW_NEW 2>&1 | Out-String
    Check '报出了第 7 个词' ($out -match '第 7 个词')
    Check '给出了修正建议' ($out -match [regex]::Escape($original)) "（应建议 $original）"
    Check '抄错时不改动文件' (Test-Open $f 'PW_MAIN')

    Write-Output ''
    Write-Output '=== 6. 别人的恢复码打不开这个文件 ==='
    # 校验和只证明「没抄错」，不证明「属于这个文件」。
    # 少了这条，用户会拿着另一个库的恢复码反复困惑
    $other = New-Fixture
    $otherCode = Join-Path $work 'other-code.txt'
    & $cli key recovery $other --password-env PW_MAIN --out $otherCode *> $null
    $f3 = Join-Path $work 'third.omy'
    $src3 = Join-Path $work 'third.txt'
    Set-Content -LiteralPath $src3 -Value 'recovery e2e payload' -Encoding UTF8 -NoNewline
    & $cli encrypt $src3 -o $f3 --password-env PW_MAIN --kdf-profile mobile --yes *> $null
    Remove-Item $src3 -Force
    $out = & $cli key restore $f3 --code-file $otherCode --new-password-env PW_NEW 2>&1 | Out-String
    Check '拒绝了不属于本文件的恢复码' ($out -match '打不开该文件')
    Check '提示区分了「没抄错」与「拿错了」' ($out -match '没抄错')

    Write-Output ''
    Write-Output '=== 7. 改密码后恢复码仍然有效（依赖槽位搬运）==='
    # 这是上一轮止血的直接收益。若 change 仍填随机，恢复码在这里就没了
    $f = New-Fixture
    & $cli key recovery $f --password-env PW_MAIN --out $codeFile *> $null
    & $cli key change $f --password-env PW_MAIN --new-password-env PW_NEW --yes *> $null
    $out = & $cli key restore $f --code-file $codeFile --new-password-env PW_SECOND 2>&1 | Out-String
    Check '改密码后恢复码没被抹掉' (Test-Open $f 'PW_SECOND') `
        "（$(($out -split "`r?`n" | Where-Object { $_ -match '打不开|error' } | Select-Object -First 1))）"
} finally {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_MAIN, Env:\PW_NEW, Env:\PW_SECOND -EA SilentlyContinue
}

Write-Output ''
Write-Output "共 $($pass + $fail) 项，失败 $fail 项"
if ($fail -gt 0) { exit 1 }
