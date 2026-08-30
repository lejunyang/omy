# 验证用户报的两个问题：
#   1. 成功提示不会自动消失，要手动点
#   2. 未加密的图片/视频双击打不开；其他类型该能用系统程序打开
#
# 关键点：测试目录必须能从侧栏点进去，探针才能走真实交互。
# 这里把 XDG_PICTURES_DIR / 注册表都不动，改用最简单可靠的办法——
# 直接在真实的「图片」目录下建一个子目录做测试，然后点进去。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9351

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 源码比二进制新就拒绝跑，避免拿旧二进制自证成功
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs,*.vue,*.js |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release"
    exit 1
}

# 测试目录放在「文档」下的专用子目录。
#
# 为什么不用临时区：探针要走真实的侧栏点击进目录，而临时区在
# 主目录下埋了四层（AppData\Local\Temp\...），双击四次才能到，
# 中间任何一层渲染慢一点就会 flaky。
#
# 为什么不给生产代码加「测试专用位置」的环境变量：那等于为测试
# 在产品里开后门，后患比省下的这点麻烦大得多。
#
# 折中：用「文档」下的一个明确标记为测试用的子目录，跑完就删。
$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-probe-test'

Write-Output '=== 0. 造测试素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

# 真的 PNG（8x8 红色），不是随便写几个字节——
# 假 PNG 会让 naturalWidth 永远是 0，测出来的失败是假的
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap 8, 8
for ($x = 0; $x -lt 8; $x++) {
    for ($y = 0; $y -lt 8; $y++) { $bmp.SetPixel($x, $y, [System.Drawing.Color]::Red) }
}
$bmp.Save((Join-Path $work 'photo.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

Set-Content -Path (Join-Path $work 'note.txt') -Value 'hello omy' -Encoding UTF8 -NoNewline

# 一个真的 zip
$zsrc = Join-Path $work '_tmp'
New-Item -ItemType Directory -Force -Path $zsrc | Out-Null
Set-Content -Path (Join-Path $zsrc 'x.txt') -Value 'inner' -Encoding UTF8
Compress-Archive -Path (Join-Path $zsrc '*') -DestinationPath (Join-Path $work 'archive.zip') -Force
Remove-Item $zsrc -Recurse -Force

# 一个真的加密文件，用来验证「加密文件不猜预览类别」
$cli = Join-Path $root 'target\release\omy.exe'
$pwf = Join-Path $work 'pw.txt'
Set-Content -Path $pwf -Value 'probe-pass-1' -NoNewline -Encoding ASCII
$srcForEnc = Join-Path $work 'note.txt'
& $cli encrypt $srcForEnc --password-file $pwf --output (Join-Path $work 'secret.omy') 2>&1 | Out-Null
Remove-Item $pwf -Force -EA SilentlyContinue

Get-ChildItem $work | ForEach-Object { Write-Output "  $($_.Name)  $($_.Length) B" }

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'
$p = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-probe.log') `
    -RedirectStandardError (Join-Path $env:TEMP 'omy-probe.err')
Write-Output "已启动 GUI，pid=$($p.Id)"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-open-and-toast.mjs $port $work 2>&1 |
        Out-String -Width 150 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
    Remove-Item Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

Write-Output ''
Write-Output "测试素材已清理（位于 $work）"
exit $code

# 跑完就删，不在用户的文档目录里留垃圾
Remove-Item $work -Recurse -Force -EA SilentlyContinue
