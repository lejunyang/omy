# omy share 端到端实测：两个进程真的配对、共享、取回。
#
# 为什么要这个：单元测试只验证纯函数，编译通过只说明类型对。
# "CLI 能不能用"必须真的把进程跑起来才知道——参数名对不对、
# 非交互密码通道是否接对了线、长驻服务能否被连上，这些在单测里
# 全都看不到。（第一版用 --kdf 就是错的，跑一次才发现。）
#
# ⚠️ 用 Start-Process + 文件重定向，不用 Start-Job：
# Job 里的 Out-String 要等进程**结束**才产出，而 serve / pair --listen
# 都是长驻进程，用 Job 永远读不到它们的输出。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$omy = Join-Path $root 'target\release\omy.exe'
$work = Join-Path $env:TEMP ('omy-cli-e2e-' + [guid]::NewGuid().ToString('N').Substring(0, 12))

$pass = 0
$fail = 0
$fails = @()
$procs = @()

function Check([bool]$ok, [string]$name, [string]$detail = '') {
    if ($ok) {
        $script:pass++
        Write-Output ("  PASS  " + $name + $(if ($detail) { "  $detail" } else { '' }))
    } else {
        $script:fail++
        $script:fails += $name
        Write-Output ("  FAIL  " + $name + $(if ($detail) { "  $detail" } else { '' }))
    }
}

# 启动一个长驻进程，输出重定向到文件
function StartBg([string[]]$argv, [string]$tag) {
    $out = Join-Path $work "$tag.out"
    $err = Join-Path $work "$tag.err"
    $p = Start-Process -FilePath $omy -ArgumentList $argv `
        -RedirectStandardOutput $out -RedirectStandardError $err `
        -PassThru -WindowStyle Hidden
    $script:procs += $p
    return $p
}

# 读一个后台进程到目前为止的全部输出（stdout + stderr）
function ReadBg([string]$tag) {
    $s = ''
    foreach ($ext in 'out', 'err') {
        $f = Join-Path $work "$tag.$ext"
        if (Test-Path $f) { $s += (Get-Content $f -Raw -EA SilentlyContinue) }
    }
    return $s
}

function KillAll {
    foreach ($p in $script:procs) {
        if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
    }
}

# ---------- 准备 ----------
New-Item -ItemType Directory -Force -Path $work | Out-Null
$shareDir = Join-Path $work 'share'
$fetchDir = Join-Path $work 'fetch'
New-Item -ItemType Directory -Force -Path $shareDir | Out-Null

$storeA = Join-Path $work 'a-devices.omy'
$storeB = Join-Path $work 'b-devices.omy'
$pwFile = Join-Path $work 'store-pw.txt'
Set-Content -Path $pwFile -Value 'store-password-123' -NoNewline -Encoding utf8
$filePw = Join-Path $work 'file-pw.txt'
Set-Content -Path $filePw -Value 'file-password-456' -NoNewline -Encoding utf8

Write-Output '=== 0. 准备一个加密文件 ==='
$plain = Join-Path $work 'secret.txt'
Set-Content -Path $plain -Value ('这是要通过局域网共享的机密内容。' * 200) -Encoding utf8
$origSize = (Get-Item $plain).Length
& $omy encrypt --password-file $filePw --kdf-profile mobile -o (Join-Path $shareDir 'secret.omy') $plain 2>&1 | Out-Null
$encExists = Test-Path (Join-Path $shareDir 'secret.omy')
Check $encExists '加密文件已生成' "原文 $origSize B"
if (-not $encExists) { Write-Output '无法继续'; exit 1 }

# ---------- 1. 设备库 ----------
Write-Output ''
Write-Output '=== 1. 设备库首次创建（非交互密码通道）==='
$out = & $omy share devices list --store $storeA --password-file $pwFile 2>&1 | Out-String
Check (Test-Path $storeA) '首次运行自动创建设备库'
Check ($out -match '尚未配对') '空设备库给出明确提示'
$out2 = & $omy --json share devices list --store $storeA --password-file $pwFile 2>&1 | Out-String
Check ($out2 -match '"count"') 'JSON 输出可用'
$badPw = Join-Path $work 'bad-pw.txt'
Set-Content -Path $badPw -Value 'wrong-password' -NoNewline -Encoding utf8
& $omy share devices list --store $storeA --password-file $badPw 2>&1 | Out-Null
Check ($LASTEXITCODE -ne 0) '错误的设备库密码必须失败'

# ---------- 2. 配对 ----------
Write-Output ''
Write-Output '=== 2. 两台"设备"配对 ==='
$pairPort = 43219
$pA = StartBg @('share', 'pair', '--listen', '--port', "$pairPort", '--store', $storeA, '--password-file', $pwFile) 'pairA'
Start-Sleep -Seconds 4

$partial = ReadBg 'pairA'
$pin = $null
if ($partial -match '配对码：(\d{6})') { $pin = $Matches[1] }
Check ($null -ne $pin) 'A 生成了 6 位配对码' $pin

$fpA = $null
if ($pin) {
    $pinFile = Join-Path $work 'pin.txt'
    Set-Content -Path $pinFile -Value $pin -NoNewline -Encoding utf8
    $outB = & $omy share pair "127.0.0.1:$pairPort" --store $storeB --password-file $pwFile --pin-file $pinFile 2>&1 | Out-String
    Check ($outB -match '已与') 'B 完成配对' (($outB -split "`n" | Where-Object { $_ -match '已与' } | Select-Object -First 1))

    # 等 A 侧写完设备库
    $waited = 0
    while (-not $pA.HasExited -and $waited -lt 15) { Start-Sleep -Seconds 1; $waited++ }
    $outA = ReadBg 'pairA'
    Check ($outA -match '已与') 'A 侧也记录了配对'

    $listA = & $omy --json share devices list --store $storeA --password-file $pwFile 2>&1 | Out-String
    $listB = & $omy --json share devices list --store $storeB --password-file $pwFile 2>&1 | Out-String
    Check ($listA -match '"fingerprint"') 'A 的设备库有已配对记录'
    Check ($listB -match '"fingerprint"') 'B 的设备库有已配对记录'
    if ($listB -match '"fingerprint":\s*"([0-9a-f]{16})"') { $fpA = $Matches[1] }
    Check ($null -ne $fpA) 'B 记录了 A 的指纹' $fpA
}

# ---------- 3. 共享与访问 ----------
Write-Output ''
Write-Output '=== 3. A 共享目录，B 连上取回 ==='
$servePort = 43220
$pS = StartBg @('share', 'serve', $shareDir, '--store', $storeA, '--password-file', $pwFile, '--port', "$servePort", '--local-only', '--no-advertise') 'serve'
Start-Sleep -Seconds 4
$serveOut = ReadBg 'serve'
Check ($serveOut -match '正在共享 1 个文件') 'A 启动共享服务' (($serveOut -split "`n" | Where-Object { $_ -match '正在共享' } | Select-Object -First 1))

if ($fpA) {
    $outC = & $omy share connect $fpA --addr "127.0.0.1:$servePort" --fetch $fetchDir --store $storeB --password-file $pwFile 2>&1 | Out-String
    Check ($outC -match '共享了 1 个文件') 'B 连上并列出文件'

    $fetched = @(Get-ChildItem -Path $fetchDir -Filter '*.omy' -EA SilentlyContinue)
    Check ($fetched.Count -eq 1) 'B 取回了 1 个文件' ("实际 " + $fetched.Count)

    if ($fetched.Count -eq 1) {
        $restored = Join-Path $work 'restored.txt'
        & $omy decrypt --password-file $filePw -o $restored $fetched[0].FullName 2>&1 | Out-Null
        $ok = Test-Path $restored
        Check $ok '取回的密文能解密还原'
        if ($ok) {
            $ha = (Get-FileHash -Path $plain -Algorithm SHA256).Hash
            $hb = (Get-FileHash -Path $restored -Algorithm SHA256).Hash
            Check ($ha -eq $hb) '还原内容与原文逐字节相同' ($hb.Substring(0, 16) + '…')
        }
    }

    # 反证：未配对的第三方连不上
    $storeC = Join-Path $work 'c-devices.omy'
    & $omy share devices list --store $storeC --password-file $pwFile 2>&1 | Out-Null
    & $omy share connect $fpA --addr "127.0.0.1:$servePort" --store $storeC --password-file $pwFile 2>&1 | Out-Null
    Check ($LASTEXITCODE -ne 0) '未配对设备无法连接（关键反证）'
}

if (-not $pS.HasExited) { Stop-Process -Id $pS.Id -Force -EA SilentlyContinue }

# ---------- 4. 吊销 ----------
Write-Output ''
Write-Output '=== 4. 吊销后立即失效 ==='
$listA2 = & $omy --json share devices list --store $storeA --password-file $pwFile 2>&1 | Out-String
$fpB = $null
if ($listA2 -match '"fingerprint":\s*"([0-9a-f]{16})"') { $fpB = $Matches[1] }
if ($fpB) {
    $outR = & $omy share devices revoke $fpB --store $storeA --password-file $pwFile 2>&1 | Out-String
    Check ($outR -match '已吊销') 'A 吊销了 B 的授权'
    $listA3 = & $omy --json share devices list --store $storeA --password-file $pwFile 2>&1 | Out-String
    Check ($listA3 -notmatch $fpB) '吊销后设备库里不再有该记录'
} else {
    Check $false '能取到 B 的指纹以便吊销'
}

KillAll
Write-Output ''
Write-Output '=============================================================='
Write-Output "结果: $pass 通过, $fail 失败"
if ($fails.Count -gt 0) {
    Write-Output '失败项:'
    foreach ($f in $fails) { Write-Output "  - $f" }
}
Write-Output '=============================================================='
if ($fail -eq 0) {
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Write-Output '(已清理临时目录)'
} else {
    Write-Output "工作目录保留以便排查: $work"
    exit 1
}
