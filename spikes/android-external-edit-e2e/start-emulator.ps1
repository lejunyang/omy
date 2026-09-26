param(
    [string]$Avd = 'omy-test-35',
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'prepare-avd.ps1') -Avd $Avd
if ($LASTEXITCODE -ne 0) { throw 'AVD 准备失败' }

function Get-ReadyEmulator {
    $lines = @(& adb devices)
    foreach ($line in $lines) {
        if ($line -match '^(emulator-\d+)\s+device$') {
            $serial = $Matches[1]
            $booted = (& adb -s $serial shell getprop sys.boot_completed 2>$null | Out-String).Trim()
            if ($booted -eq '1') { return $serial }
        }
    }
    return $null
}

$serial = Get-ReadyEmulator
if (-not $serial) {
    $avds = @(& emulator -list-avds)
    if ($LASTEXITCODE -ne 0) { throw '无法读取 Android AVD 列表' }
    if ($avds -notcontains $Avd) { throw "找不到 AVD：$Avd" }

    Start-Process -FilePath (Get-Command emulator).Source -ArgumentList @('-avd', $Avd, '-no-snapshot-save') | Out-Null
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Seconds 2
        $serial = Get-ReadyEmulator
    } while (-not $serial -and (Get-Date) -lt $deadline)
    if (-not $serial) { throw "AVD $Avd 在 $TimeoutSeconds 秒内未完成启动" }
}

& adb -s $serial shell input keyevent 82 | Out-Null
$stateDir = Join-Path $PSScriptRoot '..\..\target\android-external-edit-e2e'
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
[IO.File]::WriteAllText(
    (Join-Path $stateDir 'device.txt'),
    $serial,
    (New-Object Text.UTF8Encoding $false)
)
Write-Output $serial
