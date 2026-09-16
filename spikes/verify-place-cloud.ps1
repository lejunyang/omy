# 远程位置（WebDAV 云盘）GUI 端到端验证。
#
# 起一个真实 WebDAV 服务器（omy-remote 的 dav_server example），
# 里面放与本地库**同一份密文**的 .omy，再启动 GUI：
#   本地解锁 → 添加云盘 → 浏览 → 打开 → Range 点播 / seek → 密文缓存 → 关闭失效 → 清缓存。
# 探针走 Tauri 命令 + omystream 自定义协议，断言见 probe-place-cloud.mjs。
#
# 用 debug 产物（target\debug），跑前先：
#   cargo build -p omy-gui -p omy-cli
#   cargo build -p omy-remote --example dav_server
#   （前端有改动时先在 crates\omy-gui\frontend 跑 vite build）

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\debug\omy-gui.exe'
$omy = Join-Path $root 'target\debug\omy.exe'
$dav = Join-Path $root 'target\debug\examples\dav_server.exe'
$node = 'C:\Program Files\nodejs\node.exe'   # 绕开 osdk shim（见 PROGRESS 踩坑记录）
if (-not (Test-Path $node)) { $node = 'node' }

$davPort = 8799
$cdpPort = 9466
$pass = 'cloud-test-password'
$plainLen = 3 * 1024 * 1024 + 512 * 1024

foreach ($b in @($gui, $omy, $dav)) {
    if (-not (Test-Path $b)) { Write-Output "FAIL 缺少 $b，先按脚本头注释构建"; exit 1 }
}

$work = Join-Path $env:TEMP "omy-cloud-e2e-$PID"
$davDir = Join-Path $work 'dav'
$vaultDir = Join-Path $work 'vault'
New-Item -ItemType Directory -Force -Path $davDir, $vaultDir | Out-Null

$davProc = $null
function Stop-All {
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    if ($script:davProc) { Stop-Process -Id $script:davProc.Id -Force -ErrorAction SilentlyContinue }
}

try {
    Write-Output '=== [1] 造确定性明文 movie.mp4（3.5 MiB，不可压缩）==='
    $plain = Join-Path $work 'movie.mp4'
    & $node -e "require('fs').writeFileSync(process.argv[1], Buffer.alloc(+process.argv[2]).map((_,i)=>i%251))" $plain "$plainLen"
    if ($LASTEXITCODE -ne 0) { throw '生成明文失败' }
    Write-Output ("      {0:N0} B" -f (Get-Item $plain).Length)

    Write-Output '=== [2] CLI 加密进本地库（mobile 档，快）==='
    $env:OMY_CLOUD_PW = $pass
    $localOmy = Join-Path $vaultDir 'movie.mp4.omy'
    & $omy encrypt $plain -o $localOmy --password-env OMY_CLOUD_PW --kdf-profile mobile --thumbnail none 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw '本地加密失败' }

    Write-Output '=== [3] 同一份密文放进 WebDAV 根 + 中文子目录，再放一个普通文件 ==='
    $cloudOmy = Join-Path $davDir 'movie.mp4.omy'
    Copy-Item $localOmy $cloudOmy
    $cnDir = Join-Path $davDir '影视'
    New-Item -ItemType Directory -Force -Path $cnDir | Out-Null
    Copy-Item $localOmy (Join-Path $cnDir '大片 2.mp4.omy')
    Set-Content -Path (Join-Path $davDir 'plain.txt') -Value 'not encrypted' -NoNewline

    Write-Output '=== [4] 启动真实 WebDAV 服务器 ==='
    Get-Process dav_server -ErrorAction SilentlyContinue | Stop-Process -Force
    $davProc = Start-Process -FilePath $dav -ArgumentList "`"$davDir`"", "$davPort" -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 800
    # 自检：PROPFIND/GET 根目录能不能通
    try {
        $probe = Invoke-WebRequest -Uri "http://127.0.0.1:$davPort/movie.mp4.omy" -Method Head -UseBasicParsing
        Write-Output ("      WebDAV 就绪，movie.mp4.omy = {0:N0} B" -f $probe.Headers.'Content-Length')
    } catch { throw "WebDAV 服务器未就绪: $_" }

    Write-Output '=== [5] 启动 GUI（CDP 端口 {0}） ===' -f $cdpPort
    Get-Process omy-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    $env:OMY_GUI_CDP_PORT = "$cdpPort"
    Start-Process -FilePath $gui -WindowStyle Normal | Out-Null

    Write-Output '=== [6] 跑云盘端到端探针 ==='
    $davUrl = "http://127.0.0.1:$davPort/"
    $out = & $node --experimental-websocket (Join-Path $PSScriptRoot 'probe-place-cloud.mjs') `
        "$cdpPort" "$davUrl" "$vaultDir" "$pass" 2>&1 | Out-String
    Write-Output $out
    if ($out -notmatch 'PROBE_OK') { throw '探针未通过' }

    Write-Output ''
    Write-Output '=== 云盘端到端全部通过 ==='
}
finally {
    Stop-All
    Remove-Item Env:\OMY_CLOUD_PW -ErrorAction SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
