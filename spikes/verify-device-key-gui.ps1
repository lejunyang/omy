# 设备密钥的 GUI 端到端验证。
#
# 与 verify-device-key.ps1（CLI 层）的分工：那个验证硬件保管与解锁流程，
# 需要真人按 3 次指纹；这个验证界面接了没有，全程不触发 Hello 门禁，
# 可以无人值守跑。
#
# 后端全对而前端没接通是真实发生过的形态——挂载、状态、移除都有，
# 唯独没有任何地方用它解锁。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$port = 9374

# 先清残留进程。中途中断会留下一个仍占着调试端口的进程，
# 之后每次验证都连到那个旧进程，跑的始终是改动之前的二进制。
# 这个假象极难识破：编译确实重做了，断言也确实执行了
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

$exe = Join-Path $root 'target\release\omy-gui.exe'
if (-not (Test-Path $exe)) { Write-Output 'FAIL  先 cargo build --release -p omy-gui'; exit 1 }

# 三条新鲜度边：src→dist、dist→exe、src(rust)→exe。
# 少查一条就可能在测旧界面
$fe = Join-Path $root 'crates\omy-gui\frontend\src'
$loc = Join-Path $root 'crates\omy-gui\frontend\public\locales'
# dist 在 crates/omy-gui/dist，不在 frontend 下——tauri.conf.json 的
# frontendDist 是相对 crates/omy-gui 解析的
$dist = Join-Path $root 'crates\omy-gui\dist\app.js'
$newestSrc = Get-ChildItem $fe, $loc -Recurse -Include *.vue, *.js, *.css, *.json |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$newestRs = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1

if (-not (Test-Path $dist)) { Write-Output 'FAIL  前端未构建'; exit 1 }
if ($newestSrc.LastWriteTime -gt (Get-Item $dist).LastWriteTime) {
    Write-Output "FAIL  前端源码比 dist 新（$($newestSrc.Name)），先 bun run build"; exit 1
}
if ((Get-Item $dist).LastWriteTime -gt (Get-Item $exe).LastWriteTime) {
    Write-Output 'FAIL  dist 比 exe 新，先 cargo build --release -p omy-gui'; exit 1
}
if ($newestRs.LastWriteTime -gt (Get-Item $exe).LastWriteTime) {
    Write-Output "FAIL  rust 源码比 exe 新（$($newestRs.Name)）"; exit 1
}
Write-Output '  OK  二进制是最新构建'

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-devicekey-gui'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

# 造一个普通的加密文件，让界面有东西可以解锁
$cliExe = Join-Path $root 'target\debug\omy.exe'
Set-Content -LiteralPath (Join-Path $work '资料.txt') -Value 'body' -Encoding UTF8 -NoNewline
$env:PW_A = 'pw-alpha'
& $cliExe encrypt (Join-Path $work '资料.txt') --password-env PW_A --kdf-profile mobile --yes *> $null
Remove-Item (Join-Path $work '资料.txt') -Force -EA SilentlyContinue
Remove-Item Env:\PW_A -EA SilentlyContinue

$env:OMY_CDP_PORT = "$port"
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$port"
$proc = Start-Process -FilePath $exe -ArgumentList $work -PassThru -WindowStyle Minimized
Start-Sleep -Seconds 3

try {
    & bun (Join-Path $PSScriptRoot 'probe-device-key.mjs')
    $code = $LASTEXITCODE
} finally {
    if ($proc -and -not $proc.HasExited) { $proc | Stop-Process -Force }
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force
    Remove-Item Env:\WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -EA SilentlyContinue
    Remove-Item Env:\OMY_CDP_PORT -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}
exit $code
