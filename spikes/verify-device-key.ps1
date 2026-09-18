# 设备密钥的 CLI 端到端验证。
#
# 需要真人确认 Hello，所以不能进 CI。分成两段：
#   第一段不需要交互（status 未挂载、参数校验）
#   第二段需要确认约 3 次（add、用设备密钥解锁、remove）
#
# 跑法：pwsh -File spikes\verify-device-key.ps1

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
if (-not (Test-Path $cli)) { Write-Output 'FAIL  先 cargo build -p omy-cli'; exit 1 }

# 二进制必须比源码新，否则测的是改动之前的实现
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

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-devkey-e2e'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$src = Join-Path $work '机密.txt'
$plain = 'device key payload'
Set-Content -LiteralPath $src -Value $plain -Encoding UTF8 -NoNewline

$env:PW = 'pw-owner'
$enc = Join-Path $work '机密.omy'
& $cli encrypt $src -o $enc --slot-mode managed --password-env PW --kdf-profile mobile --yes *> $null
if (-not (Test-Path $enc)) { Write-Output 'FAIL  加密没出文件'; exit 1 }

Write-Output ''
Write-Output '--- 一、未挂载时的状态（不需要交互）---'
$st = & $cli key device $enc status 2>&1 | Out-String
Check 'status 能跑' ($LASTEXITCODE -eq 0 -or $st -match '设备密钥') $st.Trim()
Check '如实报告未挂载' ($st -match '未挂载') $st.Trim()
Check '给出了下一步怎么做' ($st -match 'device .* add|add 挂') $st.Trim()

Write-Output ''
Write-Output '--- 二、add 必须先验密码（不需要交互）---'
# 不给密码时应当失败，而不是凭「能碰到这台机器」就挂上
$env:PW_WRONG = 'not-the-password'
$bad = & $cli key device $enc add --password-env PW_WRONG --yes 2>&1 | Out-String
Check '错密码时拒绝挂载' ($bad -notmatch '已给.*挂上') $bad.Trim()
# 曾经的缺陷：from_password 只派生不验证，于是先造了硬件密钥才发现
# 密码不对——TPM 里留下一把用不到的密钥，用户还白按一次指纹。
# 这条断言盯的是「密码验证发生在造密钥之前」
Check '错密码时给出明确原因' ($bad -match '打不开该文件') $bad.Trim()
Check '错密码时没走到硬件保管那一步' ($bad -notmatch '已生成设备密钥|复用这台机器') $bad.Trim()

Write-Output ''
Write-Output '=== 以下步骤需要确认 Windows Hello ==='
Write-Output ''
Write-Output '--- 三、挂上设备密钥（会弹 Hello）---'
$add = & $cli key device $enc add --password-env PW --yes 2>&1 | Out-String
Check 'add 成功' ($add -match '已给.*挂上设备密钥') $add.Trim()
Check '警告了「不会更安全」' ($add -match '不会让文件更安全|只是省去') $add.Trim()
Check '警告了「换机器会失效」' ($add -match '永久失效|换机器') $add.Trim()

$st2 = & $cli key device $enc status 2>&1 | Out-String
Check 'status 变为已挂载' ($st2 -match '已挂载') $st2.Trim()

Write-Output ''
Write-Output '--- 四、可管理模式下槽位类型记成 device ---'
$list = & $cli key list $enc --password-env PW 2>&1 | Out-String
Check '目录里有设备密钥这一项' ($list -match '设备密钥') $list.Trim()
Check '占用数变成 2' ($list -match 'Slot 占用\s+2') $list.Trim()

Write-Output ''
Write-Output '--- 五、原密码仍然有效 ---'
# 挂设备密钥不该动原密码——它是唯一的逃生路径
$out1 = Join-Path $work 'dec-pw.txt'
& $cli decrypt $enc -o $out1 --password-env PW --yes *> $null 2>&1
Check '原密码仍能解密' (Test-Path $out1)
if (Test-Path $out1) {
    Check '解出的内容正确' ((Get-Content $out1 -Raw) -eq $plain)
}

Write-Output ''
Write-Output '--- 六、移除设备密钥（不需要 Hello）---'
$rm = & $cli key device $enc remove --yes 2>&1 | Out-String
Check 'remove 成功' ($rm -match '已移除') $rm.Trim()
$st3 = & $cli key device $enc status 2>&1 | Out-String
Check '移除后状态变回未挂载' ($st3 -match '未挂载') $st3.Trim()

# 移除只清硬件里的密钥，文件不动——原密码必须还能开
$out2 = Join-Path $work 'dec-after-rm.txt'
& $cli decrypt $enc -o $out2 --password-env PW --yes *> $null 2>&1
Check '移除设备密钥后原密码仍能开' (Test-Path $out2)

Remove-Item $work -Recurse -Force -EA SilentlyContinue
Remove-Item Env:\PW, Env:\PW_WRONG -EA SilentlyContinue

Write-Output ''
Write-Output "通过 $pass 项，失败 $fail 项"
exit $(if ($fail -eq 0) { 0 } else { 1 })
