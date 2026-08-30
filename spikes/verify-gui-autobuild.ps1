# 验证 build.rs 能在产物缺失时自动构建前端。
# 这是唯一能证明它有效的方式：静态读代码看不出 Command 能不能跑起来。
Set-Location 'E:\Projects\omy'
$dist = 'crates\omy-gui\dist'

Write-Output '=== 1. 删掉 dist，模拟全新 clone ==='
if (Test-Path $dist) {
    Remove-Item $dist -Recurse -Force
    Write-Output '  已删除 dist/'
}
Write-Output ("  dist 存在? {0}" -f (Test-Path $dist))

Write-Output ''
Write-Output '=== 2. cargo build 应当自动重建前端 ==='
# 必须 touch 一下 build.rs，否则 cargo 认为无需重跑
(Get-Item 'crates\omy-gui\build.rs').LastWriteTime = Get-Date
cargo build -p omy-gui --release 2>&1 |
    Select-String -Pattern 'warning: 正在|error|Finished|Compiling omy-gui' |
    ForEach-Object { Write-Output ("  " + $_.Line.Trim()) }

Write-Output ''
Write-Output '=== 3. 产物是否回来了 ==='
$want = @('index.html', 'app.js', 'app.css', 'locales\zh-CN.json')
$ok = $true
foreach ($f in $want) {
    $p = Join-Path $dist $f
    if (Test-Path $p) {
        Write-Output ("  PASS  {0}  ({1} B)" -f $f, (Get-Item $p).Length)
    } else {
        Write-Output ("  FAIL  缺少 {0}" -f $f)
        $ok = $false
    }
}

Write-Output ''
if ($ok) { Write-Output '结果: build.rs 自动构建前端 —— 通过' }
else { Write-Output '结果: 失败' }
