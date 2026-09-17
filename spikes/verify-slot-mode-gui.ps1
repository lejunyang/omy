# 槽位可管理模式的 GUI 端到端验证。
#
# 与 verify-slot-mode.ps1（CLI 层）的分工：那个验证格式与语义，
# 这个验证界面接对了没有。后端全对而前端把清单藏着，用户照样用不上。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$port = 9374

# 先清残留进程。中途中断会留下一个仍占着调试端口的进程，
# 之后每次验证都连到那个旧进程，跑的始终是改动之前的二进制——
# 这个假象极难识破：编译确实重做了，断言也确实执行了
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

$exe = Join-Path $root 'target\release\omy-gui.exe'
if (-not (Test-Path $exe)) { Write-Output 'FAIL  先 cargo build --release -p omy-gui'; exit 1 }

# 三条新鲜度边：src→dist、dist→exe、src→exe。
# 少查一条就可能在测旧界面
$fe = Join-Path $root 'crates\omy-gui\frontend\src'
# dist 在 crates/omy-gui/dist，不在 frontend 下——tauri.conf.json 的
# frontendDist 是相对 crates/omy-gui 解析的
$dist = Join-Path $root 'crates\omy-gui\dist\app.js'
$newestSrc = Get-ChildItem $fe -Recurse -Include *.vue,*.js,*.css |
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

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-slotmode-gui'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
Set-Content -LiteralPath (Join-Path $work '合同.txt') -Value 'contract body' -Encoding UTF8 -NoNewline

# 预先用 CLI 造一棵可管理模式的树。
#
# 走 CLI 而不是在 GUI 里一步步点：树的加密流程本身由
# verify-slot-mode.ps1 覆盖，这里要验的只是「界面读不读得到它的槽位」。
# 用 GUI 造会把两件事绑在一起，任何一环出问题都归因不清。
$cliExe = Join-Path $root 'target\debug\omy.exe'
$treeWork = Join-Path $work 'tree-managed'
$treeSrc = Join-Path $treeWork '树项目'
New-Item -ItemType Directory -Force -Path (Join-Path $treeSrc '子目录') | Out-Null
Set-Content -LiteralPath (Join-Path $treeSrc '甲.txt') -Value 'alpha' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath (Join-Path $treeSrc '子目录\乙.txt') -Value 'beta' -Encoding UTF8 -NoNewline
$env:PW_OWNER = 'pw-owner'
$env:PW_MATE = 'pw-mate'
& $cliExe encrypt $treeSrc --output-dir $treeWork --mode tree --slot-mode managed `
    --password-env PW_OWNER --kdf-profile mobile --yes *> $null
Remove-Item $treeSrc -Recurse -Force -EA SilentlyContinue
$treeRoot = Get-ChildItem $treeWork -Directory | Select-Object -First 1
if ($treeRoot) {
    & $cliExe key add $treeRoot.FullName --password-env PW_OWNER --new-password-env PW_MATE --yes *> $null 2>&1
}
Remove-Item Env:\PW_OWNER, Env:\PW_MATE -EA SilentlyContinue

$shots = Join-Path $root 'docs\screenshots\slot-mode'
New-Item -ItemType Directory -Force -Path $shots | Out-Null

$store = Join-Path $work '_devstore'
$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = $store
$proc = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 3

try {
    $out = & osdk exec --tool node@22.23.2 -- node --experimental-websocket `
        (Join-Path $root 'spikes\probe-slot-mode.mjs') $port $work $shots 2>&1 | Out-String
    Write-Output $out
    $failed = [regex]::Matches($out, '(?m)^\s+FAIL\s\s(.+)$')
    $code = if ($failed.Count -eq 0 -and $out -match '失败 0 项') { 0 } else { 1 }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force
    Remove-Item Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}
exit $code
