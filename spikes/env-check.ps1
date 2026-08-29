$ErrorActionPreference = 'Continue'
Set-Location 'E:\Projects\omy'

Write-Output '=== 工作区成员 ==='
Get-Content 'Cargo.toml' -Raw | Select-String -Pattern '(?s)members\s*=\s*\[(.*?)\]' |
    ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() }

Write-Output ''
Write-Output '=== crates 目录 ==='
Get-ChildItem 'crates' -Directory | Select-Object Name | Format-Table -AutoSize | Out-String -Width 50

Write-Output '=== spikes 子目录 ==='
Get-ChildItem 'spikes' -Directory -ErrorAction SilentlyContinue |
    Select-Object Name | Format-Table -AutoSize | Out-String -Width 50

Write-Output '=== 前端 / Tauri 环境 ==='
foreach ($c in @('node', 'npm', 'pnpm', 'yarn')) {
    $p = Get-Command $c -ErrorAction SilentlyContinue
    if ($p) {
        $v = & $c --version 2>&1 | Select-Object -First 1
        Write-Output "  $c : $v"
    } else {
        Write-Output "  $c : 未安装"
    }
}

Write-Output ''
Write-Output '=== cargo 子命令 ==='
$sub = cargo --list 2>&1 | Select-String -Pattern 'tauri|generate'
if ($sub) { $sub | Out-String -Width 80 } else { Write-Output '  未找到 cargo-tauri' }

Write-Output ''
Write-Output '=== WebView2 运行时（Windows）==='
$wv = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue
if ($wv) {
    Write-Output "  已安装 版本 $($wv.pv)"
} else {
    $wv2 = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue
    if ($wv2) { Write-Output "  已安装 版本 $($wv2.pv)" } else { Write-Output '  未检测到' }
}

Write-Output ''
Write-Output '=== 既有 spike-webview 的依赖（可复用的技术基线）==='
$sp = 'spikes\webview-range\Cargo.toml'
if (Test-Path $sp) {
    Get-Content $sp | Select-Object -First 40 | Out-String -Width 90
} else {
    Write-Output '  找不到 spikes/webview-range/Cargo.toml'
    Get-ChildItem 'spikes' -Filter 'Cargo.toml' -Recurse -ErrorAction SilentlyContinue |
        Select-Object FullName | Out-String -Width 100
}
