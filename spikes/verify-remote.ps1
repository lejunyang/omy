# 实测两台 GUI 之间的共享浏览与流式预览。
#
# 起两个真实 GUI 进程，各自用 OMY_DEVICE_STORE 隔离到不同的设备库，
# 走真实的配对握手与 Noise 信道。
#
# 为什么要两个进程：一个进程内自己连自己证明不了任何事——
# 握手双方共用同一份内存里的密钥，通不通都不说明问题。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\release\omy.exe'
$portA = 9351
$portB = 9352
$work = Join-Path $env:TEMP 'omy-remote-test'
$storeA = Join-Path $work 'devices-a.omy'
$storeB = Join-Path $work 'devices-b.omy'
$shareDir = Join-Path $work 'shared'

foreach ($exe in @($gui, $cli)) {
    if (-not (Test-Path $exe)) { Write-Output "FAIL 找不到 $exe"; exit 1 }
}

Write-Output '=== 0. 准备隔离环境与共享素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work, $shareDir | Out-Null

$pwFile = Join-Path $work 'pw.txt'
Set-Content -Path $pwFile -Value 'share-test-pw' -NoNewline -Encoding ASCII
foreach ($n in @('alpha.txt', 'beta.txt')) {
    $f = Join-Path $shareDir $n
    # 内容要够长，才能验证 Range 切片确实取的是对的那一段
    Set-Content -Path $f -Value "0123456789 content of $n padded to be long enough for range tests" -Encoding UTF8 -NoNewline
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
Start-Sleep -Milliseconds 500

# 两个实例的环境变量必须各自独立。
#
# WEBVIEW2_USER_DATA_FOLDER 也必须分开——这是实测踩出来的：
# 共用同一个用户数据目录时，第二个实例会**复用第一个的浏览器进程**，
# 它自己的 --remote-debugging-port 根本不生效。表现为进程活着、
# 日志里也打印了「CDP 已启用」，但端口就是连不上，非常难归因。
$env:OMY_DEVICE_STORE = $storeA
$env:OMY_GUI_CDP_PORT = "$portA"
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $work 'wv-a'
$pA = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-ra.log') `
    -RedirectStandardError (Join-Path $env:TEMP 'omy-ra.err')
Start-Sleep -Seconds 3

$env:OMY_DEVICE_STORE = $storeB
$env:OMY_GUI_CDP_PORT = "$portB"
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $work 'wv-b'
$pB = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-rb.log') `
    -RedirectStandardError (Join-Path $env:TEMP 'omy-rb.err')
Start-Sleep -Seconds 4

Write-Output "已启动两个 GUI: A(pid=$($pA.Id), $portA) B(pid=$($pB.Id), $portB)"
Write-Output ''

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-remote.mjs $portA $portB $shareDir 2>&1 |
        Out-String -Width 140 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $pA.Id -Force -EA SilentlyContinue
    Stop-Process -Id $pB.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_DEVICE_STORE -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
    Remove-Item Env:\WEBVIEW2_USER_DATA_FOLDER -EA SilentlyContinue
}

Write-Output ''
Write-Output '=== 隔离验证 ==='
$aExists = Test-Path $storeA
$bExists = Test-Path $storeB
Write-Output "  A 设备库落在隔离路径: $aExists"
Write-Output "  B 设备库落在隔离路径: $bExists"
$realNow = Test-Path $realStore
if ($realExistedBefore) {
    Write-Output "  真实配置目录: 测试前就存在，未做判断"
} else {
    if ($realNow) {
        Write-Output "  FAIL 污染了真实配置目录 $realStore"
        $code = 1
    } else {
        Write-Output "  真实配置目录未被创建: PASS"
    }
}
if (-not $aExists -or -not $bExists) {
    Write-Output '  FAIL 设备库没落到隔离路径'
    $code = 1
}

exit $code
