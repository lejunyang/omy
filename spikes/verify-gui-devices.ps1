# 实测 GUI 的设备与共享功能。
#
# 设备库路径用 OMY_DEVICE_STORE 隔离。
#
# 早先这里试过改写 APPDATA，**没有隔离住**：dirs::config_dir() 在
# Windows 上走 SHGetKnownFolderPath 系统调用，根本不看环境变量，
# 测试身份被写进了开发者的真实配置目录。那次事故直接促成了
# omy-net 里 OMY_DEVICE_STORE 这个开关。
$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\release\omy.exe'
$port = 9348
$work = Join-Path $env:TEMP 'omy-dev-test'
$storeFile = Join-Path $work 'devices.omy'
$shareDir = Join-Path $work 'shared'

foreach ($exe in @($gui, $cli)) {
    if (-not (Test-Path $exe)) { Write-Output "FAIL 找不到 $exe"; exit 1 }
}

Write-Output '=== 0. 造隔离环境与共享素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work, $shareDir | Out-Null

# 造两个加密文件供共享
$pwFile = Join-Path $work 'pw.txt'
Set-Content -Path $pwFile -Value 'share-test-pw' -NoNewline -Encoding ASCII
foreach ($n in @('doc-a.txt', 'doc-b.txt')) {
    $f = Join-Path $shareDir $n
    Set-Content -Path $f -Value "content of $n" -Encoding UTF8
    & $cli encrypt $f --password-file $pwFile --kdf-profile interactive --quiet 2>&1 | Out-Null
    Remove-Item $f -Force -EA SilentlyContinue
}
Remove-Item $pwFile -Force -EA SilentlyContinue
$omyCount = (Get-ChildItem $shareDir -Filter '*.omy' -File).Count
Write-Output "  共享目录: $omyCount 个加密文件"
if ($omyCount -lt 1) { Write-Output '  FAIL 素材准备失败'; exit 1 }

# 记下真实配置目录的状态，结束时验证没被污染
$realStore = Join-Path $env:APPDATA 'omy\devices.omy'
$realExistedBefore = Test-Path $realStore

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 400

$env:OMY_DEVICE_STORE = $storeFile
$env:OMY_GUI_CDP_PORT = "$port"

$err = Join-Path $env:TEMP 'omy-dev.err'
$p = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-dev.log') -RedirectStandardError $err
Write-Output "已启动 GUI，pid=$($p.Id)（设备库已隔离到 $storeFile）"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-gui-devices.mjs $port $shareDir 2>&1 |
        Out-String -Width 130 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

Write-Output ''
Write-Output '--- 隔离验证 ---'
if (Test-Path $storeFile) {
    $sz = (Get-Item $storeFile).Length
    Write-Output "  PASS  设备库写在隔离位置（$sz 字节）"
    # 反证：设备名不能明文落盘
    $bytes = [System.IO.File]::ReadAllBytes($storeFile)
    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    if ($text -match '测试机') {
        Write-Output '  FAIL  设备名明文落盘了'
        $code = 1
    } else {
        Write-Output '  PASS  设备名未明文落盘（关键反证）'
    }
} else {
    Write-Output '  FAIL  隔离位置没有设备库文件，说明环境变量没生效'
    $code = 1
}

$realExistsAfter = Test-Path $realStore
if ($realExistsAfter -ne $realExistedBefore) {
    Write-Output '  FAIL  用户真实配置目录被污染了'
    $code = 1
} else {
    Write-Output '  PASS  用户真实配置目录未被触碰（关键反证）'
}

Remove-Item $work -Recurse -Force -EA SilentlyContinue

Write-Output ''
Write-Output '--- GUI stderr ---'
if (Test-Path $err) {
    $e = Get-Content $err -Raw
    if ($e -and $e.Trim()) { Write-Output $e } else { Write-Output '  (空)' }
}
exit $code
