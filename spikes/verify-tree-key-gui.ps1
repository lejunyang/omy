# 树形改密码的 GUI 端到端验证。
#
# 走真实界面：只查前端计算属性的话，整段后端校验被删也发现不了。

$ErrorActionPreference = 'Stop'
$root = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-treekey-gui'
$repo = Split-Path $PSScriptRoot -Parent
$cli = Join-Path $repo 'target\debug\omy.exe'
$gui = Join-Path $repo 'target\release\omy-gui.exe'
$dist = Join-Path $repo 'crates\omy-gui\dist'
$fe = Join-Path $repo 'crates\omy-gui\frontend\src'

# 自证测的是最新构建。三条边都要查，漏掉任何一条都会跑到旧代码上：
# 源码 -> exe、前端源码 -> dist、dist -> exe（tauri 编译期内嵌 dist）
foreach ($pair in @(
        @('omy-gui.exe', $gui, (Join-Path $repo 'crates')),
        @('dist', (Join-Path $dist 'index.html'), $fe)
    )) {
    $art = Get-Item $pair[1] -ErrorAction SilentlyContinue
    if (-not $art) { Write-Output "FAIL 找不到 $($pair[0])"; exit 1 }
    $newest = Get-ChildItem $pair[2] -Recurse -Include *.rs, *.js, *.vue, *.css, *.json -File |
        Where-Object { $_.FullName -notmatch 'node_modules|target' } |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($newest -and $newest.LastWriteTime -gt $art.LastWriteTime) {
        Write-Output "FAIL $($newest.Name) 比 $($pair[0]) 新，先重新构建"
        exit 1
    }
}
$distIdx = Get-Item (Join-Path $dist 'index.html')
$guiExe = Get-Item $gui
if ($distIdx.LastWriteTime -gt $guiExe.LastWriteTime) {
    Write-Output 'FAIL dist 比 omy-gui.exe 新，先 cargo build --release -p omy-gui'
    exit 1
}

Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
if (Test-Path $root) { Remove-Item $root -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $root 'src\子目录') -Force | Out-Null
Set-Content -Path (Join-Path $root 'src\顶层.txt') -Value 'TOP' -NoNewline
Set-Content -Path (Join-Path $root 'src\子目录\深层.txt') -Value 'DEEP' -NoNewline
$pwf = Join-Path $root 'pw.txt'
Set-Content -Path $pwf -Value 'tree-old-pw' -NoNewline

& $cli encrypt (Join-Path $root 'src') --mode tree --password-file $pwf `
    --output-dir $root --yes 2>&1 | Out-Null
Remove-Item (Join-Path $root 'src') -Recurse -Force
Remove-Item $pwf -Force
$encDir = Get-ChildItem $root -Directory | Where-Object { $_.Name -like '*.omy' } | Select-Object -First 1
if (-not $encDir) { Write-Output 'FAIL 树形加密没产出目录'; exit 1 }

$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9378'
$proc = Start-Process -FilePath $gui -PassThru
Start-Sleep -Seconds 6

try {
    node --experimental-websocket (Join-Path $PSScriptRoot 'probe-tree-key.mjs') `
        '9378' $root $encDir.Name
    $code = $LASTEXITCODE
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    if (Test-Path $root) { Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue }
}
exit $code
