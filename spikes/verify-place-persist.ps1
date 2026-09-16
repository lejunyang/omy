# 验证远程位置的持久化：加位置 → 关应用 → 重开 → 位置还在且密码可用。
#
# 这是「凭据机器绑定」唯一靠得住的验证方式。单测能证明加解密正确，
# 但证明不了「重启后真的还能连上」——那要走完整的落盘与恢复。
#
# 同时验证反面：配置文件里不能出现明文密码。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\debug\omy-gui.exe'
$dav = Join-Path $root 'target\debug\examples\dav_server.exe'
$cfgPath = Join-Path $root 'target\debug\omy-data\config.toml'
$port = 8793
$cdp = 9471

function Stop-All {
    taskkill /F /IM omy-gui.exe 2>&1 | Out-Null
    taskkill /F /IM dav_server.exe 2>&1 | Out-Null
    Start-Sleep -Milliseconds 500
}

# 跑之前先清干净：残留的 GUI 会占着调试端口，之后每次都连到旧进程，
# 跑的始终是改动前的二进制（AGENTS.md 里记着这个坑）
Stop-All

$work = Join-Path $env:TEMP 'omy-persist-verify'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$work\dav" | Out-Null
Set-Content -Path "$work\dav\hello.txt" -Value 'hi' -NoNewline

Write-Output '=== [1] 起 WebDAV ==='
Start-Process -FilePath $dav -ArgumentList "`"$work\dav`"", "$port" -WindowStyle Hidden
Start-Sleep -Seconds 2

# 每次从干净配置开始，否则上一轮的位置会让断言失去意义
Remove-Item $cfgPath -Force -ErrorAction SilentlyContinue

$probe = Join-Path $PSScriptRoot 'probe-persist.mjs'
# 绕开 osdk shim：它没选版本时直接报错退出（见 PROGRESS 踩坑记录）
$node = 'C:\Program Files\nodejs\node.exe'
if (-not (Test-Path $node)) { $node = 'node' }

Write-Output '=== [2] 第一次启动：添加位置 ==='
$env:OMY_GUI_CDP_PORT = "$cdp"
Start-Process -FilePath $gui -WindowStyle Minimized
Start-Sleep -Seconds 6
$out1 = & $node --experimental-websocket $probe $cdp 'add' $port 2>&1 | Out-String
Write-Output $out1
if ($out1 -notmatch 'ADD_OK') { Stop-All; throw '添加阶段失败' }

# 凭据库可能真的不可用（Linux 无 Secret Service、Windows 凭据管理器存满），
# 那时正确行为是降级：位置照存，密码不存，绝不明文落盘。
# 两种环境都要能验，否则这个脚本只能在「刚好可用」的机器上跑。
$protected = $out1 -match 'SECRET_STATUS=protected'
if ($protected) {
    Write-Output '  （凭据库可用，将验证密码往返）'
} else {
    Write-Output '  （凭据库不可用，将验证降级行为：位置保留、密码不存、无明文）'
}

Stop-All
Write-Output '=== [3] 应用已退出，检查配置文件 ==='
if (-not (Test-Path $cfgPath)) { throw "配置文件没有生成：$cfgPath" }
$cfgText = [System.IO.File]::ReadAllText($cfgPath)
if ($cfgText -notmatch 'omy-persist-test') { throw '配置里没有保存这个位置' }
Write-Output '  PASS  位置已写入配置文件'

# 这一条在任何环境下都必须成立，也是整个功能的底线
if ($cfgText -match 'verify-secret-pw') {
    Write-Output $cfgText
    throw '配置文件里出现了明文密码'
}
Write-Output '  PASS  配置文件里没有明文密码'

if ($cfgText -notmatch '\[\[remote\.places\]\]') { throw '缺少 remote.places 段' }

if ($protected) {
    if ($cfgText -notmatch 'secret') { throw '凭据库可用时密码信封必须写进去' }
    Write-Output '  PASS  密码以信封形式保存'
} else {
    # 降级时绝不能留下 secret 字段——留了就说明写了个解不开的东西，
    # 或者更糟，写了明文
    if ($cfgText -match '(?m)^\s*secret\s*=') { throw '凭据库不可用时不应写入 secret 字段' }
    Write-Output '  PASS  凭据库不可用时未写入任何密码（正确降级）'
}

Write-Output '=== [4] 第二次启动：位置应自动恢复且密码可用 ==='
Start-Process -FilePath $gui -WindowStyle Minimized
Start-Sleep -Seconds 6
$out2 = & $node --experimental-websocket $probe $cdp 'verify' $port 2>&1 | Out-String
Write-Output $out2
Stop-All
if ($out2 -notmatch 'VERIFY_OK') { throw '恢复阶段失败' }

Write-Output ''
Write-Output '=== 远程位置持久化验证全部通过 ==='
