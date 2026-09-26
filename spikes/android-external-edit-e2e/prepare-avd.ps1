param(
    [string]$Avd = 'omy-test-35',
    [string]$Image = 'android-35;google_apis;x86_64',
    [string]$DataSize = '4G',
    [string]$SdcardSize = '256M'
)

$ErrorActionPreference = 'Stop'

$sdkInfo = @(& osdk android sdk-root show)
if ($LASTEXITCODE -ne 0) { throw 'osdk Android SDK 根检查失败' }
if (($sdkInfo -join "`n") -notmatch 'valid for the emulator:\s*yes') {
    throw 'osdk Android SDK 根不适用于模拟器，请先执行 osdk install'
}

$avds = @(& osdk android avd list)
if ($LASTEXITCODE -ne 0) { throw '无法读取 osdk AVD 列表' }
$entry = $avds | Where-Object { $_ -match ('^' + [regex]::Escape($Avd) + '\s+') } | Select-Object -First 1
if ($entry) {
    if ($entry -notmatch 'image=ok') {
        throw "AVD $Avd 已存在，但系统镜像不可用；请先修复工具链，或显式删除后重建"
    }
    Write-Output "AVD $Avd 已就绪"
    exit 0
}

& osdk android avd create $Avd --image $Image --data-size $DataSize --sdcard-size $SdcardSize
if ($LASTEXITCODE -ne 0) { throw "创建 AVD $Avd 失败" }

$after = @(& osdk android avd list)
$created = $after | Where-Object { $_ -match ('^' + [regex]::Escape($Avd) + '\s+.*image=ok') } | Select-Object -First 1
if (-not $created) { throw "AVD $Avd 创建后校验失败" }
Write-Output "AVD $Avd 已创建并就绪"
