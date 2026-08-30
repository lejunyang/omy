# 复现并验证：首次加密的文件必须能用同一密码打开。
#
# 用户实测报出的问题：桌面上首次加密一个文件后，用刚设的密码解不开，
# 而第二次加密同一文件却正常。根因是 GUI 构造 EncryptOptions 时漏传
# argon2，头部写的是 default(INTERACTIVE)，KEK 却按用户选的档位派生。
#
# 每个档位都在**空目录**里测，因为只有「目录里还没有 omy 文件」时
# 才走得到出问题的那条分支。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$port = 9349
$work = Join-Path $env:TEMP 'omy-kdf-test'

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }

# 源码比二进制新就拒绝运行——否则会拿着旧二进制自证成功
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$guiTime = (Get-Item $gui).LastWriteTime
if ($newestSrc.LastWriteTime -gt $guiTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name) 更新），先 cargo build --release"
    exit 1
}

Write-Output '=== 0. 造三个空目录，每个放一个待加密文件 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
foreach ($p in @('interactive', 'moderate', 'sensitive')) {
    $d = Join-Path $work $p
    New-Item -ItemType Directory -Force -Path $d | Out-Null
    # 内容随档位不同，避免误判成读到了别的文件
    Set-Content -Path (Join-Path $d "plain-$p.txt") -Value "content for $p profile" -Encoding UTF8
    Write-Output "  $d\plain-$p.txt"
}

Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Milliseconds 400

$env:OMY_GUI_CDP_PORT = "$port"
$env:OMY_DEVICE_STORE = Join-Path $work 'devices.omy'
$err = Join-Path $env:TEMP 'omy-kdf.err'
$p = Start-Process -FilePath $gui -PassThru `
    -RedirectStandardOutput (Join-Path $env:TEMP 'omy-kdf.log') -RedirectStandardError $err
Write-Output "已启动 GUI，pid=$($p.Id)"
Start-Sleep -Seconds 3

try {
    Set-Location (Join-Path $root 'spikes')
    node --experimental-websocket probe-encrypt-kdf.mjs $port $work 2>&1 |
        Out-String -Width 130 | Write-Output
    $code = $LASTEXITCODE
} finally {
    Stop-Process -Id $p.Id -Force -EA SilentlyContinue
    Get-Process omy-gui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Remove-Item Env:\OMY_GUI_CDP_PORT -EA SilentlyContinue
    Remove-Item Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

# 再用 CLI 独立复核一遍——GUI 说能开不算数，
# 换一个完全独立的实现去开才是真的
Write-Output ''
Write-Output '=== CLI 独立复核（换一条实现路径）==='
$cli = Join-Path $root 'target\release\omy.exe'
$pwFile = Join-Path $work 'pw.txt'
Set-Content -Path $pwFile -Value '132' -NoNewline -Encoding ASCII
foreach ($prof in @('interactive', 'moderate', 'sensitive')) {
    $f = Get-ChildItem (Join-Path $work $prof) -Filter '*.omy' -File -EA SilentlyContinue |
        Select-Object -First 1
    if (-not $f) { Write-Output "  FAIL $prof 没有产出 omy 文件"; $code = 1; continue }
    $out = & $cli info $f.FullName --password-file $pwFile 2>&1 | Out-String
    if ($out -match 'plain-' + $prof) {
        Write-Output "  PASS $prof CLI 也能用同一密码打开并读出文件名"
    } else {
        $firstLine = ($out.Trim() -split "`n") | Select-Object -First 1
        Write-Output "  FAIL $prof CLI 打不开：$firstLine"
        $code = 1
    }
}
Remove-Item $pwFile -Force -EA SilentlyContinue

exit $code
