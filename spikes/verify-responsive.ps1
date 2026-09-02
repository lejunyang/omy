# 验证响应式 UI：同一个 WebView 里切视口，两套 UI 各自成立。
#
# 移动端最致命的缺陷是「看着正常但点不开」——桌面靠双击打开条目，
# 触屏没有这个手势。截图和 CSS 审查都发现不了，必须真派发单击。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9358

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 自证测的是最新构建
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js, *.css |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 npm run build + cargo build --release"
    exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-responsive-test'

Write-Output ''
Write-Output '=== 0. 造测试内容 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $work 'sub-dir') | Out-Null
Set-Content -Path (Join-Path $work 'sub-dir\inner.txt') -Value 'inner' -Encoding UTF8
foreach ($n in @('a.txt', 'b.txt', 'c.txt', 'd.txt')) {
    Set-Content -Path (Join-Path $work $n) -Value "content $n" -Encoding UTF8
}
Write-Output "  1 个子目录 + 4 个文件"

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 500

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'

Write-Output ''
Write-Output '=== 1. 启动 GUI ==='
$p = Start-Process -FilePath $gui -PassThru -WindowStyle Normal
Start-Sleep -Seconds 4

$code = 1
try {
    Write-Output ''
    Write-Output '=== 2. 跑探针 ==='
    & node --experimental-websocket (Join-Path $root 'spikes\probe-responsive.mjs') $port $work 2>&1 |
        Tee-Object -FilePath (Join-Path $root 'spikes\_resp-out.txt')
    $code = $LASTEXITCODE
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
    Remove-Item Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

exit $code
