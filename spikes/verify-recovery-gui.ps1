# 恢复码 GUI 的端到端验证与截图。
#
# 与 verify-recovery.ps1（CLI 层）的分工：那个验证格式与编解码正确，
# 这个验证界面真的接对了——菜单点得动、词显示得出、错误提示到得了用户
# 眼前。两层都要有：后端全对而前端把错误压成一个码，用户照样抓瞎。
#
# 截图输出到 docs/screenshots/recovery/，不用 spikes/_shots：
# 后者匹配 .gitignore 的 /spikes/_* 会被清掉。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9370
$shots = Join-Path $root 'docs\screenshots\recovery'

if (-not (Test-Path $gui)) { Write-Output "FAIL  找不到 $gui，先 cargo build --release -p omy-gui"; exit 1 }
if (-not (Test-Path $cli)) { Write-Output "FAIL  找不到 $cli，先 cargo build -p omy-cli"; exit 1 }

# 自证跑的是最新构建。
#
# tauri.conf.json 的 frontendDist 指向 dist，release 构建会把前端**内嵌进
# 二进制**——所以改了 .vue 只跑 bun run build 是不够的，必须再 cargo build，
# 否则 exe 里装的还是上一版界面。
#
# 这个坑极具迷惑性：dist 确实是新的，前端确实构建成功了，但 GUI 显示的
# 仍是旧界面。表现为「改了代码毫无效果」，而所有中间步骤看上去都对。
# 所以这里要拿 exe 同时跟 .rs 和 dist 比。
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\|\\target\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL  二进制比源码旧（$($newestSrc.Name)），先 cargo build --release -p omy-gui"
    exit 1
}
$dist = Join-Path $root 'crates\omy-gui\dist\index.html'
if (-not (Test-Path $dist)) { Write-Output 'FAIL  前端未构建'; exit 1 }
$newestFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src'),
                          (Join-Path $root 'crates\omy-gui\frontend\public') -Recurse -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestFe.LastWriteTime -gt (Get-Item $dist).LastWriteTime) {
    Write-Output "FAIL  dist 比前端源码旧（$($newestFe.Name)），先在 frontend 下 bun run build"
    exit 1
}
# 前端是内嵌的，所以 dist 新于 exe 同样意味着跑的是旧界面
if ((Get-Item $dist).LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output 'FAIL  dist 比 exe 新——前端已内嵌进二进制，需要再 cargo build --release -p omy-gui'
    exit 1
}

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-recovery-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
New-Item -ItemType Directory -Force -Path $shots | Out-Null

$src = Join-Path $work '机密资料.txt'
Set-Content -LiteralPath $src -Value 'secret payload for recovery test' -Encoding UTF8 -NoNewline
$env:PW_MAIN = 'pw-main'
$enc = Join-Path $work 'secret.omy'
& $cli encrypt $src -o $enc --password-env PW_MAIN --kdf-profile mobile --yes *> $null
if (-not (Test-Path $enc)) { Write-Output 'FAIL  素材加密失败'; exit 1 }
Remove-Item $src -Force
Write-Output '素材：secret.omy（密码 pw-main，真名「机密资料.txt」）'

# 启动前清进程：残留的会占着调试端口，让验证连到旧进程，
# 于是跑的始终是改动之前的二进制
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

$probeOut = ''
try {
    $probeOut = & osdk exec --tool node@22.23.2 -- node --experimental-websocket `
        (Join-Path $root 'spikes\probe-recovery.mjs') $port $work $shots 2>&1 | Out-String
    Write-Output $probeOut
    $probeCode = $LASTEXITCODE
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
}

# 界面说成功不算数，用 CLI 独立复核：新密码真的能解开，明文真的没变。
# 只信界面的话，一个只改了提示语却没真写盘的实现照样"通过"
Write-Output '--- CLI 独立复核 ---'
$env:PW_THIRD = 'pw-third'
$out = Join-Path $work 'roundtrip.txt'
& $cli decrypt $enc -o $out --password-env PW_THIRD --yes *> $null
if (Test-Path $out) {
    $text = Get-Content -LiteralPath $out -Raw
    if ($text -eq 'secret payload for recovery test') {
        Write-Output '  OK  界面设的新密码能解开，且明文与原文一致'
    } else {
        Write-Output "  FAIL  明文对不上：$text"
        $probeCode = 1
    }
} else {
    Write-Output '  FAIL  用界面设的新密码解不开'
    $probeCode = 1
}

# 旧密码必须已经失效——restore 的语义是「换掉密码」，
# 若旧密码还能用，说明 keep 里多放了东西
& $cli decrypt $enc -o (Join-Path $work 'should-fail.txt') --password-env PW_MAIN --yes *> $null 2>&1
if (Test-Path (Join-Path $work 'should-fail.txt')) {
    Write-Output '  FAIL  旧密码仍然有效，restore 没有真的换掉密码'
    $probeCode = 1
} else {
    Write-Output '  OK  旧密码已失效'
}

Remove-Item $work -Recurse -Force -EA SilentlyContinue
Remove-Item Env:\PW_MAIN, Env:\PW_THIRD, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue

Write-Output ''
Write-Output "截图在 $shots"
Get-ChildItem $shots -File -EA SilentlyContinue | ForEach-Object { Write-Output "  $($_.Name)  $($_.Length) B" }

exit $(if ($probeCode -eq 0) { 0 } else { 1 })
