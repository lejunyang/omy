# 结束占用 exe 的 spike 进程并重建。
$ErrorActionPreference = 'Continue'
Get-Process -Name 'omy-spike-webview' -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Output "结束 PID $($_.Id)"
    $_.Kill()
    $_.WaitForExit(5000) | Out-Null
}
# WebView2 子进程可能仍持有句柄
Get-Process -Name 'msedgewebview2' -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowTitle -like '*omy*' } | ForEach-Object {
        Write-Output "结束 WebView2 PID $($_.Id)"; $_.Kill()
    }
Start-Sleep -Seconds 2

Set-Location 'E:\Projects\omy'
cargo build --release -p omy-spike-webview 2>&1 |
    Select-String -Pattern '^error|^warning: unused|Finished' -Context 0,5 |
    Out-String -Width 150
