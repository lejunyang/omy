# 把 Windows Hello 的确认弹窗提到最前面。
#
# 存在的理由：那个窗口常被别的窗口挡住，用户看不到就以为程序卡死了，
# 而程序其实只是在等确认。GUI 侧将来也会遇到同一个问题。
#
# 跑法：pwsh -File spikes\front-hello.ps1

$sig = @'
using System;
using System.Runtime.InteropServices;
public class OmyWin {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
}
'@
Add-Type -TypeDefinition $sig -ErrorAction SilentlyContinue

$p = Get-Process -Name 'CredentialUIBroker' -EA SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if ($p) {
    [OmyWin]::ShowWindow($p.MainWindowHandle, 9) | Out-Null   # SW_RESTORE
    [OmyWin]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Write-Output "已把「$($p.MainWindowTitle)」提到最前，请在其中确认"
} else {
    Write-Output '当前没有等待确认的 Hello 弹窗'
}
