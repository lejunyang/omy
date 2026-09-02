# 验证文件夹加密的完整链路。
#
# 造一棵真实的目录树，在 GUI 里选中它、加密、双击进容器浏览。
# 测试目录放在「文档」下的专用子目录，跑完就删——探针要走真实的
# 侧栏点击进目录，临时区埋太深，双击好几次才能到，容易 flaky。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9353

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 源码比二进制新就拒绝跑，避免拿旧二进制自证成功
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs,*.vue,*.js |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release"
    exit 1
}

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-folder-test'

Write-Output '=== 0. 造一棵测试目录树 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $work 'my-folder\inner') | Out-Null

Set-Content -Path (Join-Path $work 'my-folder\hello.txt') -Value 'hello from folder' -Encoding UTF8 -NoNewline
Set-Content -Path (Join-Path $work 'my-folder\inner\deep.txt') -Value 'deep file' -Encoding UTF8 -NoNewline

# 放一张真 PNG，验证容器里的图片能按类型识别
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap 4, 4
for ($x = 0; $x -lt 4; $x++) {
    for ($y = 0; $y -lt 4; $y++) { $bmp.SetPixel($x, $y, [System.Drawing.Color]::Blue) }
}
$bmp.Save((Join-Path $work 'my-folder\pic.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

Get-ChildItem $work -Recurse | ForEach-Object {
    Write-Output "  $($_.FullName.Substring($work.Length))  $(if($_.PSIsContainer){'<dir>'}else{"$($_.Length) B"})"
}

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
    $probe = Join-Path $root 'spikes\probe-folder-encrypt.mjs'
    # 同时留一份日志：探针输出较长，调用方的管道可能被截断，
    # 而判断「哪一项失败」需要完整输出
    $log = Join-Path $root 'spikes\_probe-out.txt'
    & node --experimental-websocket $probe $port $work 2>&1 |
        Tee-Object -FilePath $log
    $code = $LASTEXITCODE
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    # 跑完就删，不在用户的个人目录里留垃圾
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}

Write-Output ''
if ($code -eq 0) { Write-Output '全部通过' } else { Write-Output "有失败项（退出码 $code）" }
exit $code
