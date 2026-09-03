# GUI 树形模式的端到端验证：走真实界面加密，再用 CLI 解开核对。
#
# 用 CLI 解而不是 GUI 解：本轮验的是 GUI 的加密那半边，参照物要独立。
# 若两头都用 GUI，一个「两边都错得一样」的缺陷会通过。
#
# 反向也成立——CLI 加密 + GUI 解开由 verify-tree-mode.ps1 覆盖，
# 两个脚本合起来才证明两个入口产出的格式真的互通。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$omy = Join-Path $root 'target\debug\omy.exe'
$port = 9374

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

# ---- 三条新鲜度边都要查 ----
#
# 前端的依赖链是 frontend/src --(npm build)--> dist --(cargo build)--> exe，
# 而 tauri.conf.json 里 frontendDist: "dist" 意味着前端在**编译期内嵌进 exe**。
# 漏掉最后一条边的话，会跑一个内嵌着旧界面的 exe，现象是「按钮点了没反应」，
# 极像产品缺陷（这个坑实测踩过，浪费了一整轮）。
foreach ($bin in @($gui, $omy)) {
    if (-not (Test-Path $bin)) {
        Write-Output "FAIL 找不到 $bin"
        exit 1
    }
}
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
    Write-Output "FAIL omy-gui.exe 比前端产物旧（dist $($distTime.ToString('HH:mm:ss')) / exe $($binTime.ToString('HH:mm:ss'))），先 cargo build --release -p omy-gui"
    exit 1
}

$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-tree-gui-test'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null

try {
    # ---- 造素材 ----
    $src = Join-Path $work '工作资料'
    New-Item -ItemType Directory (Join-Path $src '文档') | Out-Null
    New-Item -ItemType Directory (Join-Path $src '空目录') | Out-Null
    Set-Content -Path (Join-Path $src 'readme.txt') -Value 'gui tree content' -NoNewline
    Set-Content -Path (Join-Path $src '文档\报告.md') -Value '# GUI 报告' -NoNewline
    # 同级放一个普通文件：用来验「纯文件时不出现模式选择」那条反证
    Set-Content -Path (Join-Path $work 'loose.txt') -Value 'not a folder' -NoNewline

    Write-Output ''
    Write-Output '[1] 通过 GUI 界面树形加密'

    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400

    $env:OMY_GUI_CDP_PORT = "$port"
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 4

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-tree-gui.mjs') `
        "$port" "$work" 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 探针全部通过' ($probeOut -match 'PROBE_OK') '探针输出见上'

    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500

    Write-Output ''
    Write-Output '[2] 核对 GUI 的产物'

    # 产物是目录，且名字是密文
    $encDirs = @(Get-ChildItem $work -Directory | Where-Object { $_.Name -ne '工作资料' })
    Check 'GUI 产出了加密目录' ($encDirs.Count -ge 1) "实际 $($encDirs.Count) 个"
    if ($encDirs.Count -lt 1) { throw '没有产物，后续无从核对' }
    $encRoot = $encDirs[0]
    Check '产物名是密文（不含原名）' (-not ($encRoot.Name -like '*工作资料*')) $encRoot.Name

    $allNames = (Get-ChildItem $encRoot.FullName -Recurse | ForEach-Object { $_.Name }) -join '|'
    foreach ($leak in @('工作资料', '文档', 'readme', '报告', '空目录')) {
        Check "明文名「$leak」不出现在磁盘上" (-not ($allNames -like "*$leak*")) $allNames
    }

    Write-Output ''
    Write-Output '[3] 用 CLI 解开 GUI 的产物（证明两个入口格式互通）'
    $env:OMY_TREE_GUI_PW = 'tree-gui-pw'
    $dec = Join-Path $work 'dec'
    New-Item -ItemType Directory $dec | Out-Null
    $out = & $omy decrypt $encRoot.FullName --output-dir $dec `
        --password-env OMY_TREE_GUI_PW 2>&1 | Out-String
    Check 'CLI 能解开 GUI 树形加密的产物' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE`n$out"

    $decRoot = Join-Path $dec '工作资料'
    Check '根目录名还原成原名' (Test-Path $decRoot) "找不到 $decRoot"
    Check 'readme.txt 内容一致' `
        ((Get-Content (Join-Path $decRoot 'readme.txt') -Raw) -eq 'gui tree content') ''
    Check '文档\报告.md 内容一致（中文路径与内容）' `
        ((Get-Content (Join-Path $decRoot '文档\报告.md') -Raw) -eq '# GUI 报告') ''
    Check '空目录也还原了' (Test-Path (Join-Path $decRoot '空目录')) ''

    Write-Output ''
    Write-Output "=== 合计 $pass 通过 / $fail 失败 ==="
    if ($fail -gt 0) { exit 1 }
} finally {
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_TREE_GUI_PW -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
