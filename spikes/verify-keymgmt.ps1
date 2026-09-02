# 验证密码管理（增/删/改密码）在真实界面里生效。
#
# core 的单测直接调 rewrite_slots，CLI 的 35 项走命令行，两者都绕过了
# 「按钮出现条件 → 对话框字段 → invoke 参数名 → 后端分支」这条链。
# 字段名写错的话它们照样全绿，但界面上点「应用」什么也不会发生。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9360

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

# 自证测的是最新构建。前端产物也要算进来：只改 .vue 不重新 npm run build
# 的话，跑的还是旧界面，而这类问题从截图上完全看不出来
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
    Write-Output "FAIL 前端产物比源码旧（$($newestFe.Name)），先在 frontend 里 npm run build"
    exit 1
}
Write-Output "二进制与前端产物都不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-keymgmt-test'

Write-Output ''
Write-Output '=== 0. 造测试素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

# 一个未加密文件：用来验证「选中它时不该出现密码管理按钮」
Set-Content -LiteralPath (Join-Path $work 'plain.txt') -Value 'not encrypted' -Encoding UTF8 -NoNewline

# 用 CLI 造加密文件，密码 pw-one。用 CLI 而不是走界面加密：
# 这样即使加密流程有问题也不会把密码管理的验证结果搅浑
$src = Join-Path $work 'secret-src.txt'
Set-Content -LiteralPath $src -Value 'keymgmt gui payload' -Encoding UTF8 -NoNewline
$env:PW_ONE = 'pw-one'
& $cli encrypt $src -o (Join-Path $work 'secret.omy') --password-env PW_ONE `
    --kdf-profile mobile --yes *> $null
if (-not (Test-Path (Join-Path $work 'secret.omy'))) {
    Write-Output 'FAIL 造 secret.omy 失败'
    exit 1
}
Remove-Item $src -Force
Write-Output "  secret.omy  $((Get-Item (Join-Path $work 'secret.omy')).Length) B（密码 pw-one）"
Write-Output "  plain.txt   未加密"

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
    & node --experimental-websocket (Join-Path $root 'spikes\probe-keymgmt.mjs') $port $work 2>&1 |
        Tee-Object -FilePath (Join-Path $root 'spikes\_keymgmt-out.txt')
    $code = $LASTEXITCODE

    Write-Output ''
    Write-Output '=== 3. 用 CLI 独立复核最终状态 ==='
    # 用产品自己的界面验证产品等于没验证：这里换一条完全独立的路径
    # （CLI）确认文件的最终密码状态，与探针的结论比对
    $final = Join-Path $work 'secret.omy'
    $env:PW_THREE = 'pw-three'
    $out = Join-Path $work 'roundtrip.txt'
    & $cli decrypt $final -o $out --password-env PW_THREE --yes *> $null
    if ((Test-Path $out) -and (Get-Content -LiteralPath $out -Raw).TrimEnd("`r", "`n") -eq 'keymgmt gui payload') {
        Write-Output '  PASS  CLI 用 pw-three 解出了正确明文（界面改的密码确实生效）'
    } else {
        Write-Output '  FAIL  CLI 无法用 pw-three 解出正确明文'
        $code = 1
    }
    # 载荷没被重写：文件大小应与加密时一致（改密码只动头部）
    Write-Output "  secret.omy 最终大小 $((Get-Item $final).Length) B"
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_ONE, Env:\PW_THREE, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

exit $code
