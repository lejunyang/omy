# 判断当前进程是否 elevated，并检查 WebView2 版本与 CDP 相关的策略键。
# 这决定了 CDP 打不开的原因是「运行时加固」还是别的问题。
$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$pr = New-Object System.Security.Principal.WindowsPrincipal($id)
$elev = $pr.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
Write-Output "当前进程 elevated(管理员): $elev"
Write-Output "完整性级别相关：用户 = $($id.Name)"

Write-Output ''
Write-Output '--- WebView2 运行时版本 ---'
$keys = @(
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
  'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
  'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
foreach ($k in $keys) {
  if (Test-Path $k) {
    $v = (Get-ItemProperty $k -ErrorAction SilentlyContinue).pv
    if ($v) { Write-Output "  $k -> $v" }
  }
}

Write-Output ''
Write-Output '--- 现有 AdditionalBrowserArguments 策略 ---'
foreach ($root in @('HKCU','HKLM')) {
  $p = "${root}:\Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments"
  if (Test-Path $p) {
    Write-Output "  存在 $p"
    Get-ItemProperty $p | Format-List | Out-String -Width 140 | Write-Output
  } else {
    Write-Output "  不存在 $p"
  }
}

Write-Output ''
Write-Output '--- 监听中的 92xx 端口 ---'
$conns = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
  Where-Object { $_.LocalPort -ge 9220 -and $_.LocalPort -le 9230 }
if ($conns) {
  foreach ($c in $conns) {
    $pn = (Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue).ProcessName
    Write-Output "  端口 $($c.LocalPort) <- PID $($c.OwningProcess) ($pn)"
  }
} else {
  Write-Output '  无'
}
