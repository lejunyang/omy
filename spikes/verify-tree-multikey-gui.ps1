# 树形加密文件夹在 GUI 里的**多密码与恢复码**，端到端验证并截图。
#
# 与另外两个脚本的分工，名字容易混，列清楚：
#   verify-tree-gui.ps1       GUI 加密那半边（选文件夹 → 选逐个加密 → 提交）
#   verify-tree-multikey.ps1  CLI 层的多密码与恢复码
#   本脚本                    GUI 层的多密码与恢复码
#
# 与 verify-tree-multikey.ps1（CLI 层）的分工：那个验证格式与语义正确，
# 这个验证界面接对了没有。后端全对而前端把选项灰着，用户照样用不了。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9372
$shots = Join-Path $root 'docs\screenshots\tree-multikey'

if (-not (Test-Path $gui)) { Write-Output 'FAIL  先 cargo build --release -p omy-gui'; exit 1 }
if (-not (Test-Path $cli)) { Write-Output 'FAIL  先 cargo build -p omy-cli'; exit 1 }

# 自证跑的是最新构建。
#
# frontendDist 指向 dist，release 构建会把前端**内嵌进二进制**——改了
# .vue 只跑 bun run build 是不够的，exe 里装的还是上一版界面。这个坑
# 极具迷惑性：dist 确实是新的、构建确实成功，但 GUI 显示的仍是旧界面，
# 表现为所有断言一起失败。所以要拿 exe 同时跟 .rs 和 dist 比。
$newestRs = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestRs.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL  exe 比源码旧（$($newestRs.Name)），先 cargo build --release -p omy-gui"
    exit 1
}
$dist = Join-Path $root 'crates\omy-gui\dist\index.html'
if (-not (Test-Path $dist)) { Write-Output 'FAIL  前端未构建'; exit 1 }
$newestFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src'),
                          (Join-Path $root 'crates\omy-gui\frontend\public') -Recurse -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestFe.LastWriteTime -gt (Get-Item $dist).LastWriteTime) {
    Write-Output "FAIL  dist 比前端源码旧（$($newestFe.Name)），先在 frontend 下 bun run build"
    exit 1
}
if ((Get-Item $dist).LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output 'FAIL  dist 比 exe 新——前端已内嵌进二进制，需要再 cargo build --release -p omy-gui'
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-tree-gui-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
New-Item -ItemType Directory -Force -Path $shots | Out-Null

# 造一棵有嵌套的树，名字取得像真实内容，截图才看得出「解锁后显示真名」
$src = Join-Path $work '机密项目'
New-Item -ItemType Directory -Force -Path (Join-Path $src '设计稿') | Out-Null
Set-Content -LiteralPath (Join-Path $src '需求文档.txt') -Value 'requirements' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath (Join-Path $src '设计稿\原型.txt') -Value 'prototype' -Encoding UTF8 -NoNewline

$vault = Join-Path $work 'vault'
New-Item -ItemType Directory -Force -Path $vault | Out-Null
$env:PW_TREE = 'pw-tree'
& $cli encrypt $src --output-dir $vault --mode tree --password-env PW_TREE --kdf-profile mobile --yes *> $null
$enc = Get-ChildItem $vault -Directory -EA SilentlyContinue |
    Where-Object { $_.Name -like '*.omy' } | Select-Object -First 1
if (-not $enc) { Write-Output 'FAIL  素材加密失败'; exit 1 }
Remove-Item $src -Recurse -Force
Write-Output "素材：$($enc.Name)（密码 pw-tree，真名「机密项目」，含子目录）"

# 启动前清进程：残留的会占着调试端口，让验证连到旧进程
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

$probeCode = 1
try {
    & osdk exec --tool node@22.23.2 -- node --experimental-websocket `
        (Join-Path $root 'spikes\probe-tree-multikey-gui.mjs') $port $vault $shots 2>&1 | Out-String | Write-Output
    $probeCode = $LASTEXITCODE
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
}

# 界面说成功不算数，用 CLI 独立复核整棵树真的能用新密码解开
Write-Output '--- CLI 独立复核 ---'
$env:PW_NEW = 'pw-tree-new'
$out = Join-Path $work 'roundtrip'
New-Item -ItemType Directory -Force -Path $out | Out-Null
& $cli decrypt $enc.FullName --output-dir $out --password-env PW_NEW --yes *> $null 2>&1
$f = Get-ChildItem $out -Recurse -Filter '需求文档.txt' -EA SilentlyContinue | Select-Object -First 1
$g = Get-ChildItem $out -Recurse -Filter '原型.txt' -EA SilentlyContinue | Select-Object -First 1
if ($f -and (Get-Content -LiteralPath $f.FullName -Raw) -eq 'requirements' -and
    $g -and (Get-Content -LiteralPath $g.FullName -Raw) -eq 'prototype') {
    Write-Output '  OK  界面设的新密码能解开整棵树，含嵌套目录，明文一致'
} else {
    Write-Output '  FAIL  界面说成功了，但 CLI 解不开或明文对不上'
    $probeCode = 1
}

& $cli decrypt $enc.FullName --output-dir (Join-Path $work 'shouldfail') --password-env PW_TREE --yes *> $null 2>&1
if ($LASTEXITCODE -eq 0) {
    Write-Output '  FAIL  旧密码仍然有效，restore 没有真的换掉密码'
    $probeCode = 1
} else {
    Write-Output '  OK  旧密码已失效'
}

Remove-Item $work -Recurse -Force -EA SilentlyContinue
Remove-Item Env:\PW_TREE, Env:\PW_NEW, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue

Write-Output ''
Write-Output "截图在 $shots"
Get-ChildItem $shots -File -EA SilentlyContinue | ForEach-Object { Write-Output "  $($_.Name)  $($_.Length) B" }

exit $(if ($probeCode -eq 0) { 0 } else { 1 })
