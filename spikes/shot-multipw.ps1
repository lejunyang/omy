# 截取多密码界面的真实效果。
#
# 输出目录用 docs/screenshots/multi-password/ 而不是 spikes/_shots：
# 后者匹配 .gitignore 的 /spikes/_* 规则，会被当成临时产物清掉——
# 上一轮就是这么把截好的图弄丢的，交付出去的是一批已失效的路径。

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9368
$shots = Join-Path $root 'docs\screenshots\multi-password'

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

# 自证截的是最新构建：截图最怕拿旧二进制摆拍
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js, *.css |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release -p omy-gui"
    exit 1
}

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-shot-test'
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
New-Item -ItemType Directory -Force -Path $shots | Out-Null

# 文件名取得像真实内容，截图才看得出「解锁后显示真名」这件事。
# 第三个文件用谁都不输入的密码：它证明「不属于已装入密码的文件保持锁定」，
# 只有两个文件的话看不出这一点
$a = Join-Path $work '工作周报.txt'
$b = Join-Path $work '旅行照片清单.txt'
$c = Join-Path $work '别人的文件.txt'
Set-Content -LiteralPath $a -Value 'alpha payload' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath $b -Value 'beta payload' -Encoding UTF8 -NoNewline
Set-Content -LiteralPath $c -Value 'third payload' -Encoding UTF8 -NoNewline

$env:PW_A = 'pw-alpha'
$env:PW_B = 'pw-beta'
$env:PW_C = 'pw-gamma'
$fa = Join-Path $work 'a.omy'
$fb = Join-Path $work 'b.omy'
$fc = Join-Path $work 'c.omy'

# 三个文件必须同库（--vault 复用 salt），否则演示的是「多 vault」而不是多密码
& $cli encrypt $a -o $fa --password-env PW_A --kdf-profile mobile --yes *> $null
& $cli encrypt $b -o $fb --password-env PW_B --kdf-profile mobile --vault $fa --yes *> $null
& $cli encrypt $c -o $fc --password-env PW_C --kdf-profile mobile --vault $fa --yes *> $null
Remove-Item $a, $b, $c -Force
Write-Output '素材：a.omy(pw-alpha) b.omy(pw-beta) c.omy(pw-gamma，截图里始终不解锁)'

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

try {
    & osdk exec --tool node@22.23.2 -- node --experimental-websocket `
        (Join-Path $root 'spikes\shot-multipw.mjs') $port $work $shots 2>&1
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_A, Env:\PW_B, Env:\PW_C, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

Write-Output ''
Write-Output "截图在 $shots"
Get-ChildItem $shots -File | ForEach-Object { Write-Output "  $($_.Name)  $($_.Length) B" }
