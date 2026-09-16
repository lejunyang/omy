# 树形多密码与恢复码的端到端验证。
#
# 这是方案 B（目录名两层结构）的验收：以前树只能有一个密码，多出来的
# 密码能打开每个文件却解不开目录名，解密报「文件损坏」。
#
# 注意 encrypt 的 -o 要给一个**不存在**的路径：它会把密文树建在那里。
# 给已存在的目录会撞上 rename 冲突（os error 5），那是探针写错不是产品问题。

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

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-tree-multi-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

$src = Join-Path $work 'src'
New-Item -ItemType Directory -Force -Path (Join-Path $src '子目录') | Out-Null
Set-Content -LiteralPath (Join-Path $src 'a.txt') -Value 'alpha' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath (Join-Path $src '子目录\c.txt') -Value 'charlie' -Encoding UTF8 -NoNewline

$vault = Join-Path $work 'vault'
New-Item -ItemType Directory -Force -Path $vault | Out-Null

$env:PW1 = 'pw-one'
$env:PW2 = 'pw-two'
$env:PW3 = 'pw-three'

Write-Output ''
Write-Output '--- 一、加密并添加第二个密码 ---'
$encOut = & $cli encrypt $src --output-dir $vault --mode tree --password-env PW1 --kdf-profile mobile --yes 2>&1 | Out-String
$enc = Get-ChildItem $vault -Directory -EA SilentlyContinue | Where-Object { $_.Name -like '*.omy' } | Select-Object -First 1
Check '树已加密' ($null -ne $enc) $encOut.Trim()
if (-not $enc) { Remove-Item $work -Recurse -Force -EA SilentlyContinue; exit 1 }
$encPath = $enc.FullName

$sidecars = @(Get-ChildItem $encPath -Recurse -Force -Filter '.omy-keys')
$dirs = @(Get-Item $encPath) + @(Get-ChildItem $encPath -Recurse -Directory)
Check '每个密文目录都有 .omy-keys' ($sidecars.Count -eq $dirs.Count) "边车 $($sidecars.Count) 个，目录 $($dirs.Count) 个"

$addOut = & $cli key add $encPath --password-env PW1 --new-password-env PW2 --yes 2>&1 | Out-String
Check 'key add 对目录不再被拒绝' ($addOut -notmatch '不支持目录') $addOut.Trim()

Write-Output ''
Write-Output '--- 二、两个密码都能解开整棵树 ---'
foreach ($pair in @(@{N='密码1'; E='PW1'}, @{N='密码2'; E='PW2'})) {
    $out = Join-Path $work "dec-$($pair.E)"
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $dOut = & $cli decrypt $encPath --output-dir $out --password-env $pair.E --yes 2>&1 | Out-String
    $a = Get-ChildItem $out -Recurse -Filter 'a.txt' -EA SilentlyContinue | Select-Object -First 1
    $c = Get-ChildItem $out -Recurse -Filter 'c.txt' -EA SilentlyContinue | Select-Object -First 1
    $okA = $a -and (Get-Content -LiteralPath $a.FullName -Raw) -eq 'alpha'
    $okC = $c -and (Get-Content -LiteralPath $c.FullName -Raw) -eq 'charlie'
    Check "$($pair.N) 能解开整棵树且明文正确（含嵌套）" ($okA -and $okC) $dOut.Trim()
}

Write-Output ''
Write-Output '--- 三、树形恢复码 ---'
$codeFile = Join-Path $work 'code.txt'
$recoOut = & $cli key recovery $encPath --password-env PW1 --out $codeFile --yes 2>&1 | Out-String
Check 'key recovery 对目录不再被拒绝' ($recoOut -notmatch '暂不支持目录') $recoOut.Trim()
Check '恢复码已写出' (Test-Path $codeFile) $recoOut.Trim()
if (Test-Path $codeFile) {
    $words = (Get-Content -LiteralPath $codeFile -Raw).Trim() -split '\s+'
    Check '恢复码是 26 个词' ($words.Count -eq 26) "实际 $($words.Count) 个"
}
Check '提示里说明了 .omy-keys 的后果' ($recoOut -match 'omy-keys') '没有告知删除边车的后果'

$restOut = & $cli key restore $encPath --code-file $codeFile --new-password-env PW3 --yes 2>&1 | Out-String
Check 'key restore 对目录不再被拒绝' ($restOut -notmatch '暂不支持目录') $restOut.Trim()

$outR = Join-Path $work 'dec-reco'
New-Item -ItemType Directory -Force -Path $outR | Out-Null
& $cli decrypt $encPath --output-dir $outR --password-env PW3 --yes *> $null 2>&1
$a3 = Get-ChildItem $outR -Recurse -Filter 'a.txt' -EA SilentlyContinue | Select-Object -First 1
Check '恢复码重设的新密码能解开整棵树' ($a3 -and (Get-Content -LiteralPath $a3.FullName -Raw) -eq 'alpha')

Write-Output ''
Write-Output '--- 四、目录名不因换密码而改变 ---'
Check '换密码后根目录路径不变' (Test-Path $encPath) "路径 $encPath 不在了"

Write-Output ''
Write-Output '--- 五、删掉边车的后果 ---'
$victim = Get-ChildItem $encPath -Recurse -Force -Filter '.omy-keys' | Select-Object -First 1
Remove-Item $victim.FullName -Force
$outBad = Join-Path $work 'dec-nokeys'
New-Item -ItemType Directory -Force -Path $outBad | Out-Null
$badOut = & $cli decrypt $encPath --output-dir $outBad --password-env PW3 --yes 2>&1 | Out-String
$badCode = $LASTEXITCODE
Check '缺边车时明确报错而不是静默成功' ($badCode -ne 0) "退出码 $badCode：$($badOut.Trim())"

Remove-Item $work -Recurse -Force -EA SilentlyContinue
Remove-Item Env:\PW1, Env:\PW2, Env:\PW3 -EA SilentlyContinue

Write-Output ''
Write-Output "通过 $pass 项，失败 $fail 项"
exit $(if ($fail -eq 0) { 0 } else { 1 })
