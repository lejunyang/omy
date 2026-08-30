# 实测新的文件管理器交互。
# 造一个真实目录：普通文件 + 用 omy 加密的文件，再启动 GUI 用 CDP 检查。
$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\release\omy.exe'
$port = 9346
$work = Join-Path $env:TEMP 'omy-fm-test'

foreach ($exe in @($gui, $cli)) {
    if (-not (Test-Path $exe)) { Write-Output "FAIL 找不到 $exe"; exit 1 }
}

Write-Output '=== 0. 造测试素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $work '子目录') | Out-Null

Set-Content -Path (Join-Path $work 'readme.txt') -Value 'plain text file' -Encoding UTF8
Set-Content -Path (Join-Path $work 'notes.md') -Value '# hello' -Encoding UTF8
$secret = Join-Path $work 'secret-source.txt'
Set-Content -Path $secret -Value 'this content is encrypted' -Encoding UTF8

$pwFile = Join-Path $work 'pw.txt'
Set-Content -Path $pwFile -Value 'test-password-123' -NoNewline -Encoding ASCII

& $cli encrypt $secret --password-file $pwFile --kdf-profile interactive --quiet 2>&1 | Out-Null
Remove-Item $secret -Force -EA SilentlyContinue
Remove-Item $pwFile -Force -EA SilentlyContinue

$made = Get-ChildItem $work -File | ForEach-Object { $_.Name }
Write-Output ("  素材: " + ($made -join ', '))
$omyCount = (Get-ChildItem $work -Filter '*.omy' -File).Count
if ($omyCount -lt 1) { Write-Output '  FAIL 没有生成加密文件'; exit 1 }
Write-Output "  加密文件 $omyCount 个"

Write-Output ''
Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 400

$env:OMY_GUI_CDP_PORT = "$port"
$log = Join-Path $env:TEMP 'omy-fm.log'
$err = Join-Path $env:TEMP 'omy-fm.err'
$p = Start-Process -FilePath $gui -PassThru -RedirectStandardOutput $log -RedirectStandardError $err
Write-Output "已启动 GUI，pid=$($p.Id)"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-gui-fm.mjs $port $work 2>&1 | Out-String -Width 130 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
}

Write-Output ''
Write-Output '--- GUI stderr ---'
if (Test-Path $err) {
    $e = Get-Content $err -Raw
    if ($e -and $e.Trim()) { Write-Output $e } else { Write-Output '  (空)' }
}
exit $code
