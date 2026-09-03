# 验证 GUI 的「还原到磁盘」：走真实界面把加密文件解出来。
#
# # 这个脚本要回答的问题
#
# 后端命令有单元测试、core 的解包有 9 项、CLI 那条链有 27 项，但「用户
# 点得到吗、解出来的东西对不对」全都测不到。上一轮元数据缺陷就是这么
# 躲过所有检查的：每一层单独看都对，没人测层与层之间。
#
# 素材由 CLI 加密而不是 GUI：本轮验的是**还原**，用 CLI 造素材能让
# 「解出来的内容和原件一致」这个断言的参照物是确定的。GUI 加密那半边
# 另有 verify-metadata-gui.ps1 覆盖。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
$gui = Join-Path $root 'target\release\omy-gui.exe'

if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }
if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 新鲜度守卫：拿旧二进制自证成功是最容易犯的错
$binTime = (Get-Item $gui).LastWriteTime
foreach ($c in 'omy-gui', 'omy-core') {
    $newer = Get-ChildItem (Join-Path $root "crates\$c\src") -Recurse -Filter *.rs |
        Where-Object { $_.LastWriteTime -gt $binTime }
    if ($newer) {
        Write-Output "FAIL omy-gui.exe 比源码旧（$($newer[0].Name)），先 cargo build --release -p omy-gui"
        exit 1
    }
}
# 前端的依赖链有**两条边**，都要查：
#
#     frontend/src --(npm run build)--> dist --(cargo build)--> exe
#
# tauri.conf.json 里 frontendDist: "dist"，前端产物在编译期被内嵌进 exe。
# 只查第一条边的话，会漏掉「dist 是新的但 exe 是旧的」——那时 exe 里内嵌
# 的还是上一版界面，而现象是「按钮点了没反应」，看起来完全像产品缺陷
# （实测踩过：前 10 项全过，只有对话框那几项失败，查了半天 App.vue）。
$distJs = Join-Path $root 'crates\omy-gui\dist\app.js'
if (-not (Test-Path $distJs)) {
    Write-Output "FAIL 找不到前端产物 $distJs，先在 frontend 里 npm run build"
    exit 1
}
$distTime = (Get-Item $distJs).LastWriteTime
$newerFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse -Include *.vue,*.js,*.css |
    Where-Object { $_.LastWriteTime -gt $distTime }
if ($newerFe) {
    Write-Output "FAIL 前端产物比源码旧（$($newerFe[0].Name)），先 npm run build"
    exit 1
}
if ($distTime -gt $binTime) {
    Write-Output "FAIL omy-gui.exe 比前端产物旧（dist 于 $($distTime.ToString('HH:mm:ss')) 构建，exe 于 $($binTime.ToString('HH:mm:ss'))），先 cargo build --release -p omy-gui"
    exit 1
}

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

$port = 9373
$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-restore-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$env:OMY_PW = 'restore-pw'
$proc = $null

try {
    # ---- 素材：一棵带已知 mtime 的目录树，用 CLI 加密 ----
    Write-Output "`n[1] 造素材并用 CLI 加密"
    $src = Join-Path $work 'payload'
    New-Item -ItemType Directory -Force -Path (Join-Path $src 'sub') | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $src 'empty') | Out-Null
    Set-Content -LiteralPath (Join-Path $src 'a.txt') -Value 'content-a' -NoNewline
    Set-Content -LiteralPath (Join-Path $src 'sub\b.txt') -Value 'content-bb' -NoNewline
    $times = @{
        'a.txt'     = Get-Date '2017-01-02 03:04:05'
        'sub\b.txt' = Get-Date '2018-03-04 05:06:07'
        'sub'       = Get-Date '2019-05-06 07:08:09'
        'empty'     = Get-Date '2020-07-08 09:10:11'
    }
    foreach ($k in 'a.txt', 'sub\b.txt', 'sub', 'empty') {
        (Get-Item (Join-Path $src $k)).LastWriteTime = $times[$k]
    }
    $enc = Join-Path $work 'payload.omy'
    & $cli encrypt $src -o $enc --password-env OMY_PW *> $null
    Check 'CLI 产出了加密文件' (Test-Path $enc) "$enc"
    # 原目录必须删掉：留着的话「还原出来的」和「原来就有的」分不清，
    # 一个什么都没做的实现也能让断言通过
    Remove-Item $src -Recurse -Force
    Check '原目录已移除（避免假通过）' (-not (Test-Path $src)) ''

    # 目标目录预先建好：对话框的「自选目录」需要一个存在的路径，
    # 而后端会拒绝不存在的目标（target_not_a_dir）
    $dest = Join-Path $work 'restored-here'
    New-Item -ItemType Directory -Force -Path $dest | Out-Null

    # ---- 走 GUI 界面还原 ----
    Write-Output "`n[2] 通过 GUI 界面还原"
    $env:OMY_GUI_CDP_PORT = "$port"
    # 旁路原生文件夹对话框：它是 OS 窗口，CDP 点不到
    $env:OMY_GUI_PICK_FOLDER = $dest
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 500
    # Normal 而不是最小化：最小化时 WebView 可能不渲染，元素几何全 0
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 5

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-restore-gui.mjs') $port $work 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 探针全部通过' ($probeOut -match 'PROBE_OK') "探针输出见上"

    # ---- 核对磁盘上的真实结果 ----
    Write-Output "`n[3] 核对还原结果"
    # 容器根名让它自证，不硬编码：根名丢了的话后面每条断言都会报
    # 「条目未还原」，那指向元数据，而真正的问题在索引
    $rootDirs = @(Get-ChildItem $dest -Directory -EA SilentlyContinue)
    Check '产出了唯一的容器根目录' ($rootDirs.Count -eq 1) `
        "实际有 $($rootDirs.Count) 个：$($rootDirs.Name -join ',')"
    $rroot = if ($rootDirs.Count -ge 1) { $rootDirs[0].FullName } else { $dest }
    Check '根名是原文件夹名' `
        ($rootDirs.Count -ge 1 -and $rootDirs[0].Name -eq 'payload') `
        "实际为 '$(if($rootDirs.Count -ge 1){$rootDirs[0].Name}else{'(无)'})'"

    # 内容：这是还原的根本目的
    Check 'a.txt 内容一致' `
        ((Test-Path (Join-Path $rroot 'a.txt')) -and
         (Get-Content (Join-Path $rroot 'a.txt') -Raw) -eq 'content-a') ''
    Check 'sub\b.txt 内容一致' `
        ((Test-Path (Join-Path $rroot 'sub\b.txt')) -and
         (Get-Content (Join-Path $rroot 'sub\b.txt') -Raw) -eq 'content-bb') ''
    # 空目录：不显式还原就会静默消失，而用户是有意留着它的
    Check '空目录已还原' (Test-Path (Join-Path $rroot 'empty')) ''

    # 元数据：GUI 走的是 core 的 unpack，与 CLI 同一份实现，
    # 但「同一份实现」和「GUI 真的调了它」是两件事
    foreach ($k in 'a.txt', 'sub\b.txt', 'sub', 'empty') {
        $p = Join-Path $rroot $k
        if (-not (Test-Path $p)) { Check "$k 的 mtime" $false '条目不存在'; continue }
        $got = (Get-Item $p).LastWriteTime
        $d = [Math]::Abs(($got - $times[$k]).TotalSeconds)
        Check "$k 的 mtime 经 GUI 还原后保住" ($d -lt 2) "期望 $($times[$k])，实际 $got"
    }

    # 源加密文件必须还在：还原不该消耗掉输入
    Check '加密文件仍然存在' (Test-Path $enc) '还原不应删除源文件'

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    if ($proc) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_PW -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_PICK_FOLDER -EA SilentlyContinue
}

if ($fail -gt 0) { exit 1 }
