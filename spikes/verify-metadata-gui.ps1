# 交叉验证：GUI 加密的文件夹，用 CLI 解开后元数据能否还原？
#
# GUI 与 CLI 共用 pack_folder，理论上元数据一定进了容器。但「理论上共用」
# 和「实际写进去了」是两件事——encrypt.rs 里那段注释记着：同样是共用路径，
# 漏传 argon2 就让文件当场打不开。所以这条链必须实测。
#
# 这也是本轮唯一需要在 GUI 侧验证的点：GUI 没有明文落盘的路径（实测确认
# 只有两处写盘，写的都是密文/头部），所以没有「GUI 解密还原元数据」这回事。
# 用户在 GUI 里加密、在 CLI 里解开，是元数据真正跨工具流动的路径。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
$gui = Join-Path $root 'target\release\omy-gui.exe'

if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }
if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 新鲜度守卫：omy-gui 的产物必须比 core/gui 源码新
$binTime = (Get-Item $gui).LastWriteTime
foreach ($c in 'omy-gui', 'omy-core') {
    $newer = Get-ChildItem (Join-Path $root "crates\$c\src") -Recurse -Filter *.rs |
        Where-Object { $_.LastWriteTime -gt $binTime }
    if ($newer) {
        Write-Output "FAIL omy-gui.exe 比源码旧（$($newer[0].Name)），先 cargo build --release -p omy-gui"
        exit 1
    }
}

$pass = 0
$fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

$port = 9363
$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-meta-gui-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$env:OMY_PW = 'gui-meta-pw'
$proc = $null

try {
    # ---- 素材：每个条目不同的 mtime ----
    Write-Output "`n[1] 造素材"
    $src = Join-Path $work 'folder'
    New-Item -ItemType Directory -Force -Path (Join-Path $src 'sub') | Out-Null
    Set-Content -LiteralPath (Join-Path $src 'a.txt') -Value 'aaa' -NoNewline
    Set-Content -LiteralPath (Join-Path $src 'sub\b.txt') -Value 'bbbb' -NoNewline
    $times = @{
        'a.txt'     = Get-Date '2018-02-03 04:05:06'
        'sub\b.txt' = Get-Date '2019-06-07 08:09:10'
        'sub'       = Get-Date '2020-10-11 12:13:14'
    }
    foreach ($k in 'a.txt', 'sub\b.txt', 'sub') {
        (Get-Item (Join-Path $src $k)).LastWriteTime = $times[$k]
    }
    Write-Output "  3 个条目，各自不同的 mtime"

    # ---- 用 GUI 加密 ----
    Write-Output "`n[2] 通过 GUI 界面加密该文件夹"
    # 变量名必须是 OMY_GUI_CDP_PORT。写错的话 GUI 正常起来但不开调试
    # 端口，探针只报「CDP 未就绪」，看着像启动失败
    $env:OMY_GUI_CDP_PORT = "$port"
    # 先清掉可能残留的进程：旧进程占着端口时，新进程的 CDP 连不上
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 500
    # 窗口用 Normal：最小化时 WebView 可能不渲染，元素几何全为 0，
    # 会把可点性判定判成失败
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 5

    $probeOut = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-meta-gui.mjs') $port $work 2>&1 | Out-String
    Write-Output $probeOut
    Check 'GUI 探针全部通过' ($probeOut -match 'PROBE_OK') "探针输出见上"

    # 产物名由探针回报：GUI 开启了文件名加密，落盘名是随机的，
    # 脚本自己猜不出来（早先按 folder.omy 找，把成功的加密报成失败）
    $encName = ($probeOut | Select-String -Pattern 'ENC_NAME=(.+)' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() } | Select-Object -First 1)
    $enc = if ($encName) { Join-Path $work $encName } else { $null }
    Check 'GUI 产出了加密文件' ($enc -and (Test-Path $enc)) "探针回报的名字：'$encName'"

    # ---- 用 CLI 解开，核对磁盘上的真实时间 ----
    Write-Output "`n[3] 用 CLI 解开并核对磁盘上的 mtime"
    if (Test-Path $enc) {
        $out = Join-Path $work 'restored'
        & $cli decrypt $enc -o $out --password-env OMY_PW --yes *> $null

        # 容器根名来自索引里的 root（打包时取的原文件夹名），不是密文
        # 文件名。不硬编码：让它自证，否则「容器根名丢了」会让后面每条
        # 断言都报「条目未还原」——那指向元数据还原，而真正的问题在索引
        $rootDirs = @(Get-ChildItem $out -Directory -EA SilentlyContinue)
        Check '解密产出了唯一的容器根目录' ($rootDirs.Count -eq 1) `
            "实际有 $($rootDirs.Count) 个：$($rootDirs.Name -join ',')"
        $rroot = if ($rootDirs.Count -ge 1) { $rootDirs[0].FullName } else { $out }
        Check '容器根名是原文件夹名' `
            ($rootDirs.Count -ge 1 -and $rootDirs[0].Name -eq 'folder') `
            "实际为 '$($rootDirs[0].Name)'"

        foreach ($k in $times.Keys | Sort-Object) {
            $p = Join-Path $rroot $k
            if (-not (Test-Path $p)) { Check "$k 存在" $false '未还原'; continue }
            $got = (Get-Item $p).LastWriteTime
            $d = [Math]::Abs(($got - $times[$k]).TotalSeconds)
            Check "$k 的 mtime 跨 GUI→CLI 保住" ($d -lt 2) "期望 $($times[$k])，实际 $got"
        }
        # 内容也要核，否则「时间对了但文件空了」会判成通过
        Check 'a.txt 内容' ((Get-Content (Join-Path $rroot 'a.txt') -Raw) -eq 'aaa') ''
        Check 'sub\b.txt 内容' ((Get-Content (Join-Path $rroot 'sub\b.txt') -Raw) -eq 'bbbb') ''
    }

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    if ($proc) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_PW -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
}

if ($fail -gt 0) { exit 1 }
