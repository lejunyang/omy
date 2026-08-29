# 下载 ffmpeg 便携版到项目内的 tools 目录。
#
# 为什么不用包管理器：winget 静默安装未落地，choco 需要管理员权限写
# C:\ProgramData 而被拒。便携版只写项目目录，无需任何提权，
# 且版本可控、可随项目文档记录。
$ErrorActionPreference = 'Stop'
$repo  = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $repo 'tools'
$dest  = Join-Path $tools 'ffmpeg'

if (Test-Path (Join-Path $dest 'bin\ffmpeg.exe')) {
    Write-Output "已存在: $dest\bin\ffmpeg.exe"
    & (Join-Path $dest 'bin\ffmpeg.exe') -version 2>&1 | Select-Object -First 1
    exit 0
}

New-Item -ItemType Directory -Path $tools -Force | Out-Null
$zip = Join-Path $tools 'ffmpeg.zip'

# gyan.dev 的 essentials 构建体积小且含 ffmpeg/ffprobe，
# 是 LGPL 配置需求之外的开发期工具，不进产品分发。
$urls = @(
    'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip',
    'https://github.com/GyanD/codexffmpeg/releases/download/7.1/ffmpeg-7.1-essentials_build.zip'
)

$ok = $false
foreach ($u in $urls) {
    Write-Output "尝试下载: $u"
    try {
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $u -OutFile $zip -TimeoutSec 600 -UseBasicParsing
        $ok = $true
        break
    } catch {
        Write-Output "  失败: $($_.Exception.Message)"
    }
}
if (-not $ok) { Write-Output '所有下载源均失败'; exit 1 }

$size = (Get-Item $zip).Length
Write-Output ("下载完成: {0:N0} 字节" -f $size)

Write-Output '解压中...'
$tmp = Join-Path $tools '_ffmpeg_tmp'
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
Expand-Archive -Path $zip -DestinationPath $tmp -Force

# 压缩包内是 ffmpeg-<ver>-essentials_build\bin\...，把它提上来
$inner = Get-ChildItem $tmp -Directory | Select-Object -First 1
if (-not $inner) { Write-Output '压缩包结构异常'; exit 1 }
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
Move-Item $inner.FullName $dest
Remove-Item $tmp -Recurse -Force
Remove-Item $zip -Force

$exe = Join-Path $dest 'bin\ffmpeg.exe'
if (-not (Test-Path $exe)) { Write-Output "解压后未找到 $exe"; exit 1 }
Write-Output "安装完成: $exe"
& $exe -version 2>&1 | Select-Object -First 1
