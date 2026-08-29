# 验证 .gitignore 规则：该入库的必须在，不该入库的必须被挡
Set-Location 'E:\Projects\omy'

Write-Output '=== 必须入库（被忽略就是 bug）==='
foreach ($p in @(
    'crates/omy-gui/dist/app.js',
    'crates/omy-gui/dist/locales/zh-CN.json',
    'crates/omy-gui/src/commands.rs',
    'spikes/webview-range/dist/index.html'
)) {
    git check-ignore -q $p 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Write-Output "  被忽略  $p   <-- BUG"
    } else {
        Write-Output "  会入库  $p"
    }
}

Write-Output ''
Write-Output '=== 必须挡住 ==='
foreach ($p in @(
    'crates/omy-gui/gen/schemas/desktop-schema.json',
    'spikes/peek-tail.ps1',
    'spikes/precommit-check.ps1',
    'spikes/fixtures/vault/demo-video.mp4.omy'
)) {
    git check-ignore -q $p 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Write-Output "  已挡住  $p"
    } else {
        Write-Output "  会入库  $p   <-- BUG"
    }
}
