# 验证右键菜单：删除、重命名、新建文件夹，以及各项的可用性判定。
#
# 素材里刻意混入一个树形加密目录：菜单对它的判定（不能重命名）是这一轮
# 最容易做错的地方——改掉密文目录的名字等于把那段密文扔了，里面的文件
# 全在但名字永远解不开。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$omy = Join-Path $root 'target\debug\omy.exe'
$port = 9377
$pw = 'ctxmenu-pw'

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

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-ctxmenu-test'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null

# 「逃逸」是路径穿越反证用的名字。如果后端校验真的失效，它会被建到
# **上一级**目录里，而 finally 只清 $work——残渣会让此后每次运行的反证
# 断言都失败，且失败信息指向产品。所以开头和结尾都要清。
$escaped = Join-Path (Split-Path -Parent $work) '逃逸'
Remove-Item -Recurse -Force $escaped -ErrorAction SilentlyContinue

try {
    Write-Output '[1] 造素材'
    Set-Content -Path (Join-Path $work 'plain.txt') -Value 'rename me' -NoNewline
    Set-Content -Path (Join-Path $work '删我.txt') -Value 'trash me' -NoNewline

    # 混一个树形加密目录进来，用来验「加密目录不能重命名」
    $src = Join-Path $work '机密'
    New-Item -ItemType Directory $src | Out-Null
    Set-Content -Path (Join-Path $src 'inner.txt') -Value 'secret' -NoNewline
    $env:OMY_CTX_PW = $pw
    & $omy encrypt $src --mode tree --output-dir $work --password-env OMY_CTX_PW 2>&1 | Out-Null
    Check 'CLI 树形加密成功' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
    Remove-Item -Recurse -Force $src
    $encRoot = Get-ChildItem $work -Directory | Where-Object { $_.Name -match '\.omy$' }
    Check '有一个密文名目录做素材' ($null -ne $encRoot) ''

    Write-Output ''
    Write-Output '[2] 跑探针'
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400

    $env:OMY_GUI_CDP_PORT = "$port"
    Start-Process -FilePath $gui -WindowStyle Normal | Out-Null
    Start-Sleep -Seconds 4

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-context-menu.mjs') `
        "$port" "$work" 2>&1 | Out-String
    Write-Output $probeOut
    Check '右键菜单探针全部通过' ($probeOut -match 'PROBE_OK') '探针输出见上'

    Write-Output ''
    Write-Output '[3] 回收站与磁盘状态'
    # 「移到回收站」必须真的不在原处了
    Check '删除的文件已不在磁盘上' (-not (Test-Path (Join-Path $work '删我.txt'))) ''
    # 重命名的结果要落到磁盘，而不只是界面显示变了
    Check '重命名已落到磁盘' (Test-Path (Join-Path $work '改过的名字.txt')) ''
    Check '旧名字已不存在' (-not (Test-Path (Join-Path $work 'plain.txt'))) ''
    Check '新建的文件夹在磁盘上' (Test-Path (Join-Path $work '新目录')) ''
    # 反证：非法名字不能建出任何东西
    Check '非法名字没有建出目录' (-not (Test-Path (Join-Path $work '逃逸'))) ''
    Check '非法名字没有逃出工作目录' `
        (-not (Test-Path (Join-Path (Split-Path -Parent $work) '逃逸'))) `
        '上一级目录里出现了「逃逸」——路径校验被绕过'

    Write-Output ''
    Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
    if ($fail -gt 0) { exit 1 }
    Write-Output '全部通过'
} finally {
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_CTX_PW -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    # 见上：路径校验失效时建出来的残渣，不清会污染后续运行
    Remove-Item -Recurse -Force $escaped -ErrorAction SilentlyContinue
}
