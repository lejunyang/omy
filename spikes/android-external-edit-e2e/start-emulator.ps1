param(
    [string]$Avd = 'omy-test-35',
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'prepare-avd.ps1') -Avd $Avd
if ($LASTEXITCODE -ne 0) { throw 'AVD 准备失败' }

$adbCommand = Get-Command adb.exe -All -ErrorAction Stop |
    Where-Object { $_.CommandType -eq 'Application' } |
    Select-Object -First 1
if (-not $adbCommand) { throw '找不到 osdk 暴露的 adb.exe' }
$adb = $adbCommand.Source

function Get-ReadyEmulator {
    $lines = @(& $adb devices)
    foreach ($line in $lines) {
        if ($line -match '^(emulator-\d+)\s+device$') {
            $serial = $Matches[1]
            $booted = (& $adb -s $serial shell getprop sys.boot_completed 2>$null | Out-String).Trim()
            if ($booted -eq '1') { return $serial }
        }
    }
    return $null
}

$serial = Get-ReadyEmulator
if (-not $serial) {
    # prepare-avd.ps1 已经通过 osdk 校验 AVD 存在且镜像可用，这里不要再
    # 经过 emulator.cmd shim 查询一次；shim 适合前台转发，不适合作为 GUI
    # 子进程入口。
    $stateDir = Join-Path $PSScriptRoot '..\..\target\android-external-edit-e2e'
    New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
    $stdoutLog = Join-Path $stateDir 'emulator.stdout.log'
    $stderrLog = Join-Path $stateDir 'emulator.stderr.log'
    foreach ($log in @($stdoutLog, $stderrLog)) {
        if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log -Force }
    }

    # Get-Command emulator 会优先返回 osdk 的 .cmd shim。把 cmd shim 交给
    # Start-Process 时子进程会立刻退出，父脚本却只能空等到超时；显式选 osdk
    # 暴露的 emulator.exe，既保留项目工具链版本，也能可靠跟踪进程退出状态。
    $emulatorCommand = Get-Command emulator.exe -All -ErrorAction Stop |
        Where-Object { $_.CommandType -eq 'Application' } |
        Select-Object -First 1
    if (-not $emulatorCommand) { throw '找不到 osdk 暴露的 emulator.exe' }
    $emulatorProcess = Start-Process -FilePath $emulatorCommand.Source `
        -ArgumentList @('-avd', $Avd, '-no-snapshot-save') `
        -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Seconds 2
        $emulatorProcess.Refresh()
        if ($emulatorProcess.HasExited) {
            $detail = if (Test-Path -LiteralPath $stderrLog) {
                [IO.File]::ReadAllText($stderrLog, (New-Object Text.UTF8Encoding $false, $true)).Trim()
            } else { '' }
            throw "AVD $Avd 启动进程提前退出（exit=$($emulatorProcess.ExitCode)）：$detail"
        }
        $serial = Get-ReadyEmulator
    } while (-not $serial -and (Get-Date) -lt $deadline)
    if (-not $serial) { throw "AVD $Avd 在 $TimeoutSeconds 秒内未完成启动；日志：$stderrLog" }
}

& $adb -s $serial shell input keyevent 82 | Out-Null
$stateDir = Join-Path $PSScriptRoot '..\..\target\android-external-edit-e2e'
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
[IO.File]::WriteAllText(
    (Join-Path $stateDir 'device.txt'),
    $serial,
    (New-Object Text.UTF8Encoding $false)
)
Write-Output $serial

# 办公自动化环境会在任务进程结束时回收其子进程。交互验收需要把模拟器留给
# 用户继续查看，因此仅在显式开启时让 osdk 任务与 Emulator 同寿命；常规 E2E
# 不设置此变量，启动任务仍会正常返回，不阻塞依赖链。
if ($env:OMY_EMULATOR_KEEP_RUNNING -eq '1' -and $emulatorProcess) {
    while (-not $emulatorProcess.HasExited) {
        Start-Sleep -Seconds 2
        $emulatorProcess.Refresh()
    }
}
