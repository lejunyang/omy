# 检查 ffmpeg 是否可用；若已安装但不在当前 PATH，输出其真实位置。
$machine = [System.Environment]::GetEnvironmentVariable('Path', 'Machine')
$user    = [System.Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path = "$machine;$user"

$p = Get-Command ffmpeg -ErrorAction SilentlyContinue
if ($p) {
    Write-Output "ffmpeg -> $($p.Source)"
    & $p.Source -version 2>&1 | Select-Object -First 1
    exit 0
}

Write-Output 'ffmpeg 不在 PATH，搜索常见安装位置：'
$cands = @()
foreach ($root in @(
    "$env:LOCALAPPDATA\Microsoft\WinGet\Packages",
    'C:\Program Files',
    'C:\ProgramData\chocolatey\bin'
)) {
    if (Test-Path $root) {
        $cands += Get-ChildItem $root -Recurse -Filter 'ffmpeg.exe' -ErrorAction SilentlyContinue -Depth 4
    }
}
if ($cands.Count -eq 0) {
    Write-Output '  未找到 ffmpeg.exe'
    exit 1
}
foreach ($c in $cands | Select-Object -First 5) {
    Write-Output "  $($c.FullName)"
}
