# 验证「重新加密」（密钥轮换）在真实界面里生效。
#
# 与 verify-keymgmt.ps1 的分工：那个验 add/change/remove（只改文件头），
# 这个验 reencrypt（换文件密钥、重写载荷）。两者对「载荷有没有变」的断言
# 方向正好相反，混在一个脚本里容易写串。
#
# 素材要够大：轮换的进度条是这次的重点，小文件会在一次回调里跑完，
# 进度条一闪而过，探针轮询不到——那不是产品缺陷，是素材选得不对。

$ErrorActionPreference = 'Continue'
$root = 'E:\Projects\omy'
$gui = Join-Path $root 'target\release\omy-gui.exe'
$cli = Join-Path $root 'target\debug\omy.exe'
$port = 9362

if (-not (Test-Path $gui)) { Write-Output "FAIL 找不到 $gui"; exit 1 }
if (-not (Test-Path $cli)) { Write-Output "FAIL 找不到 $cli"; exit 1 }

# 自证测的是最新构建。前端产物也要算进来：只改 .vue 不重新 npm run build
# 的话，跑的还是旧界面，而这类问题从截图上完全看不出来
$newestSrc = Get-ChildItem (Join-Path $root 'crates') -Recurse -Include *.rs, *.vue, *.js, *.css |
    Where-Object { $_.FullName -notmatch '\\node_modules\\|\\dist\\' } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestSrc.LastWriteTime -gt (Get-Item $gui).LastWriteTime) {
    Write-Output "FAIL 二进制比源码旧（$($newestSrc.Name)），先 cargo build --release -p omy-gui"
    exit 1
}
$distJs = Join-Path $root 'crates\omy-gui\dist\app.js'
if (-not (Test-Path $distJs)) { Write-Output 'FAIL 找不到前端产物 dist\app.js'; exit 1 }
$newestFe = Get-ChildItem (Join-Path $root 'crates\omy-gui\frontend\src') -Recurse -Include *.vue, *.js, *.css |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($newestFe.LastWriteTime -gt (Get-Item $distJs).LastWriteTime) {
    Write-Output "FAIL 前端产物比源码旧（$($newestFe.Name)），先在 frontend 里 npm run build"
    exit 1
}
# locale 是运行时从 public 读的，不进 dist，但改了文案不重启 GUI 同样看不到
Write-Output "二进制与前端产物都不比源码旧（最新源码 $($newestSrc.Name)）"

$docs = [Environment]::GetFolderPath('MyDocuments')
$work = Join-Path $docs 'omy-reenc-test'

Write-Output ''
Write-Output '=== 0. 造测试素材 ==='
Remove-Item $work -Recurse -Force -EA SilentlyContinue
New-Item -ItemType Directory -Force -Path $work | Out-Null

Set-Content -LiteralPath (Join-Path $work 'plain.txt') -Value 'not encrypted' -Encoding UTF8 -NoNewline

# 造一个约 24 MB 的载荷：进度条要能被探针轮询到，就必须跨足够多的分块。
# 内容用随机字节，避免压缩把它压到几 KB——那样又变回"一闪而过"
$src = Join-Path $work 'secret-src.bin'
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$buf = New-Object byte[] (1024 * 1024)
$fs = [IO.File]::Create($src)
try {
    for ($i = 0; $i -lt 24; $i++) { $rng.GetBytes($buf); $fs.Write($buf, 0, $buf.Length) }
} finally { $fs.Dispose() }
$plainSize = (Get-Item $src).Length
$plainHash = (Get-FileHash -LiteralPath $src -Algorithm SHA256).Hash

$env:PW_ONE = 'pw-one'
& $cli encrypt $src -o (Join-Path $work 'secret.omy') --password-env PW_ONE `
    --kdf-profile mobile --yes *> $null
if (-not (Test-Path (Join-Path $work 'secret.omy'))) {
    Write-Output 'FAIL 造 secret.omy 失败'
    exit 1
}
Remove-Item $src -Force
$encPath = Join-Path $work 'secret.omy'
$sizeBefore = (Get-Item $encPath).Length
# 记下轮换前的载荷指纹，最后独立复核它确实变了
$bytesBefore = [IO.File]::ReadAllBytes($encPath)
$hlen = [BitConverter]::ToUInt32($bytesBefore, 12)
$uuidBefore = ($bytesBefore[16..31] | ForEach-Object { $_.ToString('x2') }) -join ''
Write-Output "  secret.omy  $sizeBefore B（明文 $plainSize B，密码 pw-one）"
Write-Output "  file_uuid   $($uuidBefore.Substring(0,16))…"
Write-Output "  plain.txt   未加密"

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
    & node --experimental-websocket (Join-Path $root 'spikes\probe-reencrypt.mjs') $port $work 2>&1 |
        Tee-Object -FilePath (Join-Path $root 'spikes\_reenc-out.txt')
    $code = $LASTEXITCODE

    Write-Output ''
    Write-Output '=== 3. 用 CLI 独立复核最终状态 ==='
    # 用产品自己的界面验证产品等于没验证：换一条完全独立的路径（CLI）
    # 确认轮换的三个关键性质
    $env:PW_TWO = 'pw-two'
    $out = Join-Path $work 'roundtrip.bin'
    & $cli decrypt $encPath -o $out --password-env PW_TWO --yes *> $null
    if (Test-Path $out) {
        $rtHash = (Get-FileHash -LiteralPath $out -Algorithm SHA256).Hash
        if ($rtHash -eq $plainHash) {
            Write-Output '  PASS  CLI 用 pw-two 解出的明文与原文哈希一致'
        } else {
            Write-Output "  FAIL  明文哈希不一致（轮换破坏了内容）"
            $code = 1
        }
    } else {
        Write-Output '  FAIL  CLI 无法用 pw-two 解开轮换后的文件'
        $code = 1
    }

    $bytesAfter = [IO.File]::ReadAllBytes($encPath)
    $uuidAfter = ($bytesAfter[16..31] | ForEach-Object { $_.ToString('x2') }) -join ''
    if ($uuidBefore -ne $uuidAfter) {
        Write-Output "  PASS  file_uuid 已更换（$($uuidBefore.Substring(0,8)) -> $($uuidAfter.Substring(0,8))）"
    } else {
        Write-Output '  FAIL  file_uuid 没变，说明没有真的轮换'
        $code = 1
    }

    # 载荷必须整体改变。这是轮换与「只改密码」的实质区别，
    # 也是这个脚本存在的理由
    $same = $true
    for ($i = $hlen; $i -lt $bytesBefore.Length; $i++) {
        if ($bytesBefore[$i] -ne $bytesAfter[$i]) { $same = $false; break }
    }
    if (-not $same) {
        Write-Output '  PASS  载荷密文已整体改变（不是只改了文件头）'
    } else {
        Write-Output '  FAIL  载荷密文没变，退化成了 keyslot 改写'
        $code = 1
    }

    if ($bytesAfter.Length -eq $sizeBefore) {
        Write-Output "  PASS  文件大小不变（$sizeBefore B，格式参数被保留）"
    } else {
        Write-Output "  FAIL  文件大小从 $sizeBefore 变成 $($bytesAfter.Length)，格式参数可能丢了"
        $code = 1
    }
} finally {
    Get-Process -Id $p.Id -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $work -Recurse -Force -EA SilentlyContinue
    Remove-Item Env:\PW_ONE, Env:\PW_TWO, Env:\OMY_GUI_CDP_PORT, Env:\OMY_DEVICE_STORE -EA SilentlyContinue
}

exit $code
