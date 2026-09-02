# 验证「加密后移到回收站」在真实界面里生效。
#
# 单测直接调 handle_original，绕过了「对话框选项 → 请求字段 → 后端分支」
# 这条链；字段名传丢也照样绿。这里走真实界面，并在最后用 Shell API
# 查回收站，确认原件是可还原的而不是被永久删除。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9356

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release"
    exit 1
}
Write-Output "二进制不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-trash-test'

Write-Output ''
Write-Output '=== 0. 造测试文件 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
$victim = Join-Path $work 'to-trash.txt'
Set-Content -Path $victim -Value 'move me to the recycle bin' -Encoding UTF8 -NoNewline
Write-Output "  to-trash.txt  $((Get-Item $victim).Length) B"

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
    & node --experimental-websocket (Join-Path $root 'spikes\probe-trash.mjs') $port $work 2>&1 |
        Tee-Object -FilePath (Join-Path $root 'spikes\_trash-out.txt')
    $code = $LASTEXITCODE

    Write-Output ''
    Write-Output '=== 3. 回收站里能不能找回来 ==='
    # 只判「原件消失」不够：永久删除也满足，而那是数据丢失。
    # 用 Shell API 列回收站，确认确实可还原。
    $shell = New-Object -ComObject Shell.Application
    $bin = $shell.Namespace(10)
    $found = $false
    foreach ($item in $bin.Items()) {
        if ($item.Name -eq 'to-trash.txt' -or $item.Name -eq 'to-trash') { $found = $true; break }
    }
    if ($found) {
        Write-Output '  PASS  回收站里找到了 to-trash.txt（可还原，不是永久删除）'
    } else {
        Write-Output '  FAIL  回收站里没找到 to-trash.txt'
        $code = 1
    }
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}

exit $code
