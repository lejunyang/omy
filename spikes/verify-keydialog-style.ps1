$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cli = Join-Path $root 'target\debug\omy.exe'
$gui = Join-Path $root 'target\release\omy-gui.exe'
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }
if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 新鲜度守卫：本次改的是 CSS 与 Vue，只重建其一都会让验证跑在旧页面上
$binTime = (Get-Item $gui).LastWriteTime
$dist = Join-Path $root 'crates\omy-gui\frontend\dist\index.html'
if (Test-Path $dist) {
    $distTime = (Get-Item $dist).LastWriteTime
    $newerFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse |
        Where-Object { -not $_.PSIsContainer -and $_.LastWriteTime -gt $distTime }
    if ($newerFe) {
        Write-Output "FAIL 前端产物比源码旧（$($newerFe[0].Name)），先 npm run build"
        exit 1
    }
    if ($distTime -gt $binTime) {
        Write-Output 'FAIL exe 比前端产物旧，先 cargo build --release -p omy-gui'
        exit 1
    }
}

$pass = 0; $fail = 0
function Check($name, $cond, $detail) {
    if ($cond) { $script:pass++; Write-Output "  PASS $name" }
    else { $script:fail++; Write-Output "  FAIL $name  $detail" }
}

$port = 9381
$work = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-keystyle-test'
$plain = Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'omy-keystyle-src'
Remove-Item $work, $plain -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work, $plain | Out-Null
$proc = $null
try {
    Write-Output "`n[1] 造一个加密文件"
    Add-Type -AssemblyName System.Drawing
    $bmp = New-Object System.Drawing.Bitmap 320, 240
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.FillRectangle([System.Drawing.Brushes]::SteelBlue, 0, 0, 320, 240)
    $g.Dispose()
    $bmp.Save((Join-Path $plain 'pic.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $env:OMY_KS_PW = 'keystyle-pw'
    & $cli encrypt (Join-Path $plain 'pic.png') --output-dir $work `
        --password-env OMY_KS_PW --kdf-profile mobile --thumbnail auto --yes *> $null
    $enc = (Get-ChildItem $work -Filter *.omy).Count
    Check '加密文件已产出' ($enc -ge 1) "实际 $enc 个"

    Write-Output "`n[2] 检查密码管理弹窗的选项与样式"
    $env:OMY_GUI_CDP_PORT = "$port"
    taskkill /F /IM omy-gui.exe *> $null
    Start-Sleep -Milliseconds 600
    $proc = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
    Start-Sleep -Seconds 5
    $out = & node --experimental-websocket (Join-Path $PSScriptRoot 'probe-keydialog-style.mjs') `
        $port $work 2>&1 | Out-String
    Write-Output $out
    Check '密码管理弹窗探针全部通过' ($out -match 'PROBE_OK') '探针输出见上'

    Write-Output "`n=== 合计 $pass 通过 / $fail 失败 ==="
} finally {
    if ($proc) { Stop-Process -Id $proc.Id -Force -EA SilentlyContinue }
    taskkill /F /IM omy-gui.exe *> $null
    Remove-Item $work, $plain -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT, Env:\OMY_KS_PW -EA SilentlyContinue
}
if ($fail -gt 0) { exit 1 }
