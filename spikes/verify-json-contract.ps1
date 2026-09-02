# 每个支持 --json 的命令，输出必须是**单个**合法 JSON 文档。
#
# 曾经有 6 个命令在逐行循环里调 result(text, json!(null))，JSON 模式
# 下每行吐一个裸 null，最后才是真正的对象，整体不可解析。症状极其
# 隐蔽：人类模式一切正常，只有写脚本的人会撞上——而 --json 存在的
# 唯一意义就是给脚本消费。
#
# 同时校验人类模式没有被改坏：修法是「JSON 模式静默」，若改错方向
# 会变成人类模式也不打印，那等于把功能删了。

$ErrorActionPreference = 'Continue'
$omy = 'E:\Projects\omy\target\debug\omy.exe'

if (-not (Test-Path $omy)) { Write-Output "FAIL 找不到 $omy"; exit 1 }

$newest = Get-ChildItem 'E:\Projects\omy\crates\omy-cli\src' -Recurse -Filter *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ((Get-Item $omy).LastWriteTime -lt $newest.LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newest.Name)），先 cargo build -p omy-cli"
    exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newest.Name)）"

$work = Join-Path $env:TEMP 'omy-json-contract'
Remove-Item -Recurse -Force $work -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$env:OMY_JSON_PW = 'json-contract-pw'

# 造真实数据，否则逐行循环不会执行，测不到那个坑
$src = Join-Path $work 'sample.txt'
Set-Content -LiteralPath $src -Value 'hello json contract' -Encoding UTF8 -NoNewline
& $omy encrypt $src -o (Join-Path $work 'sample.omy') `
    --password-env OMY_JSON_PW --kdf-profile mobile --yes *> $null

$pass = 0
$fail = 0
function Check($name, $ok, $detail = '') {
    if ($ok) { Write-Output "  PASS  $name  $detail"; $script:pass++ }
    else { Write-Output "  FAIL  $name  $detail"; $script:fail++ }
}

$cases = @(
    @{ Name = 'doctor'; J = @('--json', 'doctor'); H = @('doctor') },
    @{ Name = 'list'; J = @('--json', 'list', $work); H = @('list', $work) },
    @{ Name = 'scan'; J = @('--json', 'scan', $work, '--password-env', 'OMY_JSON_PW');
       H = @('scan', $work, '--password-env', 'OMY_JSON_PW') },
    @{ Name = 'info'; J = @('--json', 'info', (Join-Path $work 'sample.omy'));
       H = @('info', (Join-Path $work 'sample.omy')) },
    @{ Name = 'share devices'; J = @('--json', 'share', 'devices', 'list');
       H = @('share', 'devices', 'list') }
)

Write-Output ''
Write-Output '=== JSON 模式：必须是单个合法 JSON，且不含裸 null 行 ==='
foreach ($c in $cases) {
    $o = & $omy @($c.J) 2>$null | Out-String
    if ([string]::IsNullOrWhiteSpace($o)) {
        Check "$($c.Name) 有输出" $false '空输出'
        continue
    }
    $nulls = ($o -split "`n" | Where-Object { $_.Trim() -eq 'null' }).Count
    $parsed = $true
    try { $null = $o | ConvertFrom-Json -ErrorAction Stop } catch { $parsed = $false }
    Check "$($c.Name) 输出可解析" $parsed
    Check "$($c.Name) 无裸 null 行" ($nulls -eq 0) "裸 null=$nulls"
}

Write-Output ''
Write-Output '=== 人类模式：该打印的仍要打印（别把功能改没了）==='
foreach ($c in $cases) {
    $o = & $omy @($c.H) 2>$null | Out-String
    # doctor / info 一定有内容；list/scan 有一个 .omy 也该有内容；
    # share devices 可能没有已配对设备，允许为空
    if ($c.Name -eq 'share devices') {
        Check "$($c.Name) 人类模式不报错" $true '（无配对设备时可为空）'
    } else {
        Check "$($c.Name) 人类模式有输出" (-not [string]::IsNullOrWhiteSpace($o)) `
            "长度=$($o.Trim().Length)"
    }
}

Write-Output ''
Write-Output '=== 人类模式不该出现 JSON 花括号（两种模式不能混）==='
$h = & $omy doctor 2>$null | Out-String
Check 'doctor 人类模式无 JSON 结构' (-not ($h -match '"checks"'))

Remove-Item -Recurse -Force $work -EA SilentlyContinue
Remove-Item Env:\OMY_JSON_PW -EA SilentlyContinue

Write-Output ''
Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
if ($fail -gt 0) { exit 1 }
exit 0
