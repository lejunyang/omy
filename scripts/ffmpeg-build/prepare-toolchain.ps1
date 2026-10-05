# 准备 FFmpeg 构建所需的工具链，并算出 build-windows.sh 需要的 SYSROOT。
#
# 为什么要有这层：build-windows.sh 内部**不能**调用 osdk。嵌套的 osdk 会尝试
# 交互提示并卡在等 stdin 上（实测挂了 18 分钟只烧掉 4.8 CPU 秒，看起来像死锁，
# 极难判断）。所以凡是需要问 osdk 的事情都在这里做完，再通过环境变量传进去。
#
# 用法：
#   pwsh -File scripts\ffmpeg-build\prepare-toolchain.ps1
#   # 然后按它打印的命令跑 build-windows.sh
#
# 本脚本兼容 PowerShell 5.1，但仓库约定用 pwsh 7 执行（见 AGENTS.md）。
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

Write-Output '=== 检查 osdk ==='
$osdk = Get-Command osdk -ErrorAction SilentlyContinue
if (-not $osdk) {
    Write-Output '找不到 osdk。工具链由项目根目录的 osdk.toml 提供，请先安装 osdk。'
    exit 1
}
Write-Output ("  osdk: " + $osdk.Source)

# osdk.toml 含 conda: 工具，属于 trust-required。未信任时该目录下所有 osdk
# 命令都会失败（连 pwsh profile 里的 activate 都会报错），所以先确认。
Push-Location $repo
try {
    $probe = & osdk -q where conda:gcc_win-64 2>&1 | Out-String
    if ($probe -match 'not trusted') {
        Write-Output ''
        Write-Output 'osdk.toml 尚未信任。请先执行：'
        Write-Output ("  osdk --yes trust " + (Join-Path $repo 'osdk.toml'))
        Write-Output '（注意：每次编辑该文件都会使信任哈希失效，需要重新信任）'
        exit 1
    }

    Write-Output ''
    Write-Output '=== 确认构建所需命令均可用 ==='
    # 这些命令由 osdk.toml 的 [settings.shims.tools.*].expose 提供。
    # m2-base 是元包，默认不生成任何 shim（否则 msys 版 ls/test/sort 会盖住
    # Windows 同名命令），所以缺失往往意味着 expose 漏了，而不是包没装。
    $need = @(
        @('bash', '--version'), @('make', '--version'), @('sed', '--version'),
        @('nasm', '-v'), @('pkg-config', '--version'), @('bsdtar', '--version'),
        @('cmake', '--version'), @('ninja', '--version'),
        @('x86_64-w64-mingw32-gcc', '-dumpversion'),
        @('x86_64-w64-mingw32-nm', '--version')
    )
    $missing = @()
    foreach ($n in $need) {
        $line = ''
        try { $line = (& $n[0] $n[1] 2>&1 | Select-Object -First 1) } catch { }
        $line = ("$line" -replace '\s+', ' ').Trim()
        # 「多个工具都提供该命令」也算不可用：那说明有重复安装的包需要卸掉
        if ($line -eq '' -or $line -match 'refusing|not recognized|no version') {
            $missing += $n[0]
            Write-Output ("  {0,-24} 不可用" -f $n[0])
        } else {
            $show = if ($line.Length -gt 40) { $line.Substring(0, 40) } else { $line }
            Write-Output ("  {0,-24} {1}" -f $n[0], $show)
        }
    }
    if ($missing.Count -gt 0) {
        Write-Output ''
        Write-Output ('以下命令不可用：' + ($missing -join ', '))
        Write-Output '排查顺序：'
        Write-Output '  1. osdk current            —— 确认工具确实来自项目配置'
        Write-Output '  2. osdk reshim             —— 若报归属冲突，说明有重复包需卸载'
        Write-Output '  3. 检查 osdk.toml 的 expose 是否列了该命令'
        exit 1
    }

    # sysroot 要转成 bash 能懂的 POSIX 路径：E:\x -> /E/x
    $gccPrefix = (& osdk -q where conda:gcc_win-64 2>&1 | Out-String).Trim() -split "`r?`n" |
        Select-Object -First 1
    $posix = ($gccPrefix -replace '\\', '/') -replace '^([A-Za-z]):', '/$1'
    $sysroot = $posix + '/Library/x86_64-w64-mingw32/sysroot'

    # CRT 在 <sysroot>/usr/lib 而非 <sysroot>/lib，这是最容易误判成
    # 「包没装全」的一个坑，所以在这里就确认清楚。
    $crt = Join-Path $gccPrefix 'Library\x86_64-w64-mingw32\sysroot\usr\lib\crt2.o'
    Write-Output ''
    Write-Output '=== sysroot ==='
    Write-Output ("  " + $sysroot)
    if (Test-Path $crt) {
        Write-Output '  crt2.o 已找到（configure 需要 -B <sysroot>/usr/lib 才能定位它）'
    } else {
        Write-Output '  警告：未找到 crt2.o，链接会失败'
    }

    Write-Output ''
    Write-Output '=== 下一步 ==='
    Write-Output '在项目根目录执行：'
    Write-Output ''
    Write-Output ("  `$env:SYSROOT='" + $sysroot + "'")
    Write-Output '  bash scripts/ffmpeg-build/build-windows.sh'
    Write-Output ''
} finally {
    Pop-Location
}
