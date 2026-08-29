# Spike S1/S5 环境探测。
param()

Write-Output '=== 工具可用性 ==='
foreach ($c in @('ffmpeg', 'ffprobe', 'node', 'pnpm')) {
    $p = Get-Command $c -ErrorAction SilentlyContinue
    if ($p) { Write-Output ("{0,-10} {1}" -f $c, $p.Source) }
    else    { Write-Output ("{0,-10} 未安装" -f $c) }
}

Write-Output ''
Write-Output '=== 可用的系统示例视频 ==='
$found = @()
foreach ($d in @("$env:PUBLIC\Videos", "$env:WINDIR\Performance\WinSAT")) {
    if (Test-Path $d) {
        $found += Get-ChildItem $d -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Extension -in '.mp4', '.wmv', '.avi', '.mkv' }
    }
}
if ($found.Count -eq 0) {
    Write-Output '未找到系统示例视频'
} else {
    $found | Select-Object -First 8 | ForEach-Object {
        Write-Output ("{0,12:N0} B  {1}" -f $_.Length, $_.FullName)
    }
}

Write-Output ''
Write-Output '=== WebView2 缓存根（S5 需要检查的位置）==='
foreach ($d in @(
    "$env:LOCALAPPDATA\Microsoft\Edge\User Data",
    "$env:LOCALAPPDATA\Temp"
)) {
    if (Test-Path $d) { Write-Output ("存在  " + $d) } else { Write-Output ("缺失  " + $d) }
}
