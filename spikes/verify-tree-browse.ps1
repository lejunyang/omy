# 验证「树形加密的目录能像普通文件夹一样浏览」。
#
# 素材用 CLI 加密：本轮验的是浏览，参照物要确定。GUI 加密那半边由
# verify-tree-gui.ps1 覆盖。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$omy = Join-Path $root 'target\debug\omy.exe'
$port = 9375
$pw = 'browse-tree-pw'

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

foreach ($bin in @($gui, $omy)) {
    if (-not (Test-Path $bin)) { Write-Output "FAIL 找不到 $bin"; exit 1 }
}
# 三条新鲜度边：src→dist、dist→exe、src→exe。中间那条是 tauri 把 dist
# 编译期内嵌带来的，漏掉会跑内嵌旧界面的 exe，现象是「点了没反应」
$binTime = (Get-Item $gui).LastWriteTime
$newerRs = Get-ChildItem (Join-Path $root 'crates') -Recurse -Filter *.rs |
    Where-Object { $_.LastWriteTime -gt $binTime }
if ($newerRs) {
    Write-Output "FAIL omy-gui.exe 比源码旧（$($newerRs[0].Name)），先 cargo build --release -p omy-gui"
    exit 1
}
$distJs = Join-Path $root 'crates\omy-gui\dist\app.js'
$distTime = (Get-Item $distJs).LastWriteTime
$newerFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse `
    -Include *.vue, *.js, *.css | Where-Object { $_.LastWriteTime -gt $distTime }
if ($newerFe) {
    Write-Output "FAIL 前端产物比源码旧（$($newerFe[0].Name)），先 npm run build"
    exit 1
}
if ($distTime -gt $binTime) {
    Write-Output "FAIL omy-gui.exe 比前端产物旧，先 cargo build --release -p omy-gui"
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-browse-test'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null

try {
    Write-Output '[1] 用 CLI 造一棵树形加密目录'
    $src = Join-Path $work '工作资料'
    New-Item -ItemType Directory (Join-Path $src '文档') | Out-Null
    Set-Content -Path (Join-Path $src 'readme.txt') -Value 'browse me' -NoNewline
    Set-Content -Path (Join-Path $src '文档\报告.md') -Value '# 报告正文' -NoNewline

    $env:OMY_BROWSE_PW = $pw
    & $omy encrypt $src --mode tree --output-dir $work --password-env OMY_BROWSE_PW 2>&1 | Out-Null
    Check 'CLI 树形加密成功' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"

    # 原目录删掉：留着的话「列出来的」可能是原目录而不是解出来的名字，
    # 一个什么都没做的实现也能让断言通过
    Remove-Item -Recurse -Force $src
    $encRoot = (Get-ChildItem $work -Directory)[0]
    Check '产物是密文名目录' ($encRoot.Name -match '^[A-Z2-7~]+\.omy$') $encRoot.Name
    Check '原目录已删除（避免假通过）' (-not (Test-Path $src)) ''

    Write-Output ''
    Write-Output '[2] 通过 GUI 浏览它'
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400

    $env:OMY_GUI_CDP_PORT = "$port"
    Start-Process -FilePath $gui -WindowStyle Normal | Out-Null
    Start-Sleep -Seconds 4

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-tree-browse.mjs') `
        "$port" "$work" "$pw" 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 浏览探针全部通过' ($probeOut -match 'PROBE_OK') '探针输出见上'

    Write-Output ''
    Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
    if ($fail -gt 0) { exit 1 }
} finally {
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_BROWSE_PW -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
