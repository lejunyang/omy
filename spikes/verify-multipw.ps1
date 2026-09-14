# 验证「一个会话同时装多个密码」在真实界面里生效。
#
# core 的 6 项集成测试直接调 add_password，绕过了整条
# 「对话框 → invoke → commands::unlock → scan → 列表 → 渲染」。
# 链上任何一环退回替换语义，core 测试照样全绿，而用户输第二个密码时
# 第一批文件会当场锁上——正是本功能要解决的问题本身。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9366

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

# 自证测的是最新构建。前端产物也要算：只改 .vue 不重新 build 的话，
# 跑的还是旧界面，而这从截图上完全看不出来
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js, *.css |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release -p omy-gui"
    exit 1
}
$distJs = Join-Path $root 'crates\omy-gui\dist\app.js'
if (-not (Test-Path $distJs)) { Write-Output 'FAIL 找不到前端产物 dist\app.js'; exit 1 }
$newestFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse -Include *.vue, *.js, *.css |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestFe.LastWriteTime -gt (Get-Item $distJs).LastWriteTime) {
    Write-Output "FAIL 前端产物比源码旧（$($newestFe.Name)），先在 frontend 里跑一次 build"
    exit 1
}
# 文案也要算进来：add_more_hint / already_loaded 就在 locales 里，
# 漏了它们会让界面显示成键名而断言仍可能通过
$newestLoc = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\public\locales') -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestLoc -and $newestLoc.LastWriteTime -gt (Get-Item $distJs).LastWriteTime) {
    Write-Output "警告 文案比产物新（$($newestLoc.Name)）——locales 是 public 静态资源，通常由 build 复制"
}
Write-Output "二进制与前端产物都不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-multipw-test'

Write-Output ''
Write-Output '=== 0. 造测试素材：同一个 vault，两个不同密码各一个文件 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

# 关键：两个文件必须在**同一个 vault**（共用 vault_salt），否则测的是
# 「多 vault」而不是「多密码」——前者早就支持了，会假通过。
# 用 --vault 让第二个文件复用第一个的 salt 与 KDF 参数
$srcA = Join-Path $work 'real-a.txt'
$srcB = Join-Path $work 'real-b.txt'
Set-Content -LiteralPath $srcA -Value 'payload alpha' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath $srcB -Value 'payload beta' -Encoding UTF8 -NoNewline

$env:PW_ALPHA = 'pw-alpha'
$env:PW_BETA = 'pw-beta'
$fileA = Join-Path $work 'a.omy'
$fileB = Join-Path $work 'b.omy'

& $cli encrypt $srcA -o $fileA --password-env PW_ALPHA --kdf-profile mobile --yes *> $null
if (-not (Test-Path $fileA)) { Write-Output 'FAIL 造 a.omy 失败'; exit 1 }
& $cli encrypt $srcB -o $fileB --password-env PW_BETA --kdf-profile mobile --vault $fileA --yes *> $null
if (-not (Test-Path $fileB)) { Write-Output 'FAIL 造 b.omy 失败'; exit 1 }
Remove-Item $srcA, $srcB -Force

# 自证两个文件确实同库：salt 不同的话整个测试没有意义。
#
# 直接读文件头字节而不是 `info --json`：salt 是密码学材料，JSON 输出
# 刻意不含它（核实过 info.rs）。按格式规范 §2，vault_salt 在偏移 32、
# 长 16 字节
function Get-VaultSalt([string]$path) {
    $fs = [System.IO.File]::OpenRead($path)
    try {
        $buf = New-Object byte[] 48
        $n = $fs.Read($buf, 0, 48)
        if ($n -lt 48) { return $null }
        return ($buf[32..47] | ForEach-Object { $_.ToString('x2') }) -join ''
    } finally { $fs.Dispose() }
}
$saltA = Get-VaultSalt $fileA
$saltB = Get-VaultSalt $fileB
if (-not $saltA -or $saltA -ne $saltB) {
    Write-Output "FAIL 两个文件不在同一个 vault（$saltA vs $saltB），测的就不是多密码了"
    exit 1
}
Write-Output "  a.omy（pw-alpha）与 b.omy（pw-beta）同库，salt=$($saltA.Substring(0, 12))…"

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

Write-Output ''
Write-Output '=== 1. 启动 GUI ==='
$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

$code = 1
try {
    Write-Output ''
    Write-Output '=== 2. 跑探针 ==='
    # node 经 osdk exec 取用，不往仓库 osdk.toml 写版本固定
    & osdk exec --tool node@22.23.2 -- node --experimental-websocket (Join-Path $root 'spikes\probe-multipw.mjs') $port $work 2>&1 |
        Tee-Object -FilePath (Join-Path $root 'spikes\_multipw-out.txt')
    $code = $LASTEXITCODE

    Write-Output ''
    Write-Output '=== 3. 用 CLI 独立复核两个文件各自的密码 ==='
    # 用产品界面验证产品等于没验证。这里换一条独立路径确认：
    # 两个文件确实各归各的密码，界面上「同时可见」不是因为它们共用了密码
    $outA = Join-Path $work 'out-a.txt'
    $outB = Join-Path $work 'out-b.txt'
    & $cli decrypt $fileA -o $outA --password-env PW_ALPHA --yes *> $null
    & $cli decrypt $fileB -o $outB --password-env PW_BETA --yes *> $null
    $okA = (Test-Path $outA) -and ((Get-Content -LiteralPath $outA -Raw).TrimEnd("`r", "`n") -eq 'payload alpha')
    $okB = (Test-Path $outB) -and ((Get-Content -LiteralPath $outB -Raw).TrimEnd("`r", "`n") -eq 'payload beta')
    if ($okA -and $okB) {
        Write-Output '  PASS  两个文件分别只能用自己的密码解出正确明文'
    } else {
        Write-Output "  FAIL  CLI 复核失败（a=$okA b=$okB）"
        $code = 1
    }

    # 反向确认：a 的密码打不开 b。不测这条的话，万一两个文件都挂了
    # 两个密码（比如 --vault 顺带复制了 slot），界面上的「同时可见」
    # 就与多密码毫无关系，而上面的断言照样全绿
    $cross = Join-Path $work 'cross.txt'
    & $cli decrypt $fileB -o $cross --password-env PW_ALPHA --yes *> $null 2>&1
    if (Test-Path $cross) {
        Write-Output '  FAIL  pw-alpha 竟然能解开 b.omy——两个文件挂了同一批密码，本测试无效'
        $code = 1
    } else {
        Write-Output '  PASS  pw-alpha 打不开 b.omy（确认是两个独立密码）'
    }
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    # 残留进程会占着调试端口，下次验证会连到旧进程、跑的是改动前的二进制
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_ALPHA, Env:\PW_BETA, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

exit $code
