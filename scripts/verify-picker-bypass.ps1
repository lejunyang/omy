# 验证 pick_folder 的测试旁路只在设了环境变量时生效。
#
# 测试全程都设着 OMY_GUI_PICK_FOLDER，等于从没验证过「不设时会走真实对话框」。
# 万一旁路写成无条件返回某个默认值，测试照样全绿，而用户点「浏览」
# 会拿到一个莫名其妙的固定路径。
#
# 判据：不设变量时命令会挂起等用户操作（弹窗没人点），超时即为正确行为。
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\omy-gui.exe'
$port = 9455

if (-not (Test-Path $exe)) { Write-Output "找不到 $exe"; exit 1 }

Get-Process -Name 'omy-gui' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

# 关键：只设 CDP 端口，**不设** OMY_GUI_PICK_FOLDER
$env:OMY_GUI_CDP_PORT = "$port"
Remove-Item Env:\OMY_GUI_PICK_FOLDER -ErrorAction SilentlyContinue

$log = Join-Path $env:TEMP 'omy-bypass-out.log'
$errlog = Join-Path $env:TEMP 'omy-bypass-err.log'
$p = Start-Process -FilePath $exe -RedirectStandardOutput $log `
    -RedirectStandardError $errlog -PassThru

$listening = $false
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    if (Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue) {
        $listening = $true; break
    }
    if ($p.HasExited) { break }
}
if (-not $listening) {
    Write-Output 'FAIL: GUI 未能启动'
    if (-not $p.HasExited) { $p | Stop-Process -Force }
    exit 1
}

Write-Output '已启动（未设 OMY_GUI_PICK_FOLDER）'
Write-Output '调用 pick_folder，期望它挂起等待用户（而非立刻返回值）...'

$node = Join-Path $repo 'spikes\probe-nobypass.mjs'
$out = & node $node $port 2>&1 | Out-String
Write-Output $out.Trim()

# 无论结果如何都要收尾：留着一个弹了框的 GUI 进程会挡住后续测试
Get-Process -Name 'omy-gui' -ErrorAction SilentlyContinue | Stop-Process -Force

if ($out -match 'CORRECT') {
    Write-Output ''
    Write-Output 'PASS  不设环境变量时不走旁路（真实对话框被调起）'
    exit 0
} else {
    Write-Output ''
    Write-Output 'FAIL  旁路可能无条件生效，或命令行为异常'
    exit 1
}
