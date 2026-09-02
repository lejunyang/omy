# 验证 GUI 加密进度条的完整链路。
#
# 造一个足够大的文件（小文件几十毫秒就加密完，进度条一闪而过采不到样），
# 在真实界面里选中并加密，用探针在**加密过程中**高频采样进度条状态。
#
# 测试目录放「文档」下，与既有探针一致——探针要走真实侧栏点击进目录。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9354

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 源码比二进制新就拒绝跑，避免拿旧二进制自证成功
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release"
    exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-progress-test'

Write-Output ''
Write-Output '=== 0. 造一个大文件 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

# 96 MiB：够跑几秒，能采到多帧进度；内容不可高度压缩，
# 否则压缩太快、进度条一闪而过
$big = Join-Path $work 'big-file.bin'
$fs = [System.IO.File]::Create($big)
$rand = [System.Random]::new(7)
$buf = New-Object byte[] 1048576
foreach ($i in 1..96) {
    $rand.NextBytes($buf)
    $fs.Write($buf, 0, $buf.Length)
}
$fs.Close()
Write-Output ("  big-file.bin  {0:N0} 字节" -f (Get-Item $big).Length)

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

Write-Output ''
Write-Output '=== 1. 启动 GUI ==='
$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

try {
    Write-Output ''
    Write-Output '=== 2. 跑探针 ==='
    $probe = Join-Path $root 'spikes\probe-encrypt-progress.mjs'
    $log = Join-Path $root 'spikes\_prog-out.txt'
    & node --experimental-websocket $probe $port $work 2>&1 | Tee-Object -FilePath $log
    $code = $LASTEXITCODE
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}

exit $code
