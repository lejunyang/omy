# Android 构建环境体检：明确「缺什么、装了之后能到哪一步」。
#
# 不做安装，只做核实。Android SDK/NDK 是 GB 级下载且需要接受
# 许可协议，属于用户环境决策，不应由脚本擅自代劳。
#
# 用法：pwsh -NoProfile -File spikes\check-android-env.ps1

$ErrorActionPreference = 'Continue'
$ok = 0
$miss = 0

function Need($name, $present, $how) {
    if ($present) {
        Write-Output "  [有]  $name"
        $script:ok++
    } else {
        Write-Output "  [缺]  $name"
        Write-Output "        $how"
        $script:miss++
    }
}

Write-Output '=== Android 构建前置条件 ==='
Write-Output ''

$javaOk = [bool](Get-Command java -ErrorAction SilentlyContinue)
Need 'JDK 17+（Gradle 需要）' $javaOk `
    '装 Android Studio 会一并带上，或 winget install EclipseAdoptium.Temurin.17.JDK'

$sdk = $env:ANDROID_HOME
if (-not $sdk) { $sdk = $env:ANDROID_SDK_ROOT }
$sdkOk = $sdk -and (Test-Path $sdk)
Need 'Android SDK（ANDROID_HOME 指向它）' $sdkOk `
    '装 Android Studio，SDK Manager 里勾 Platform 34 + Build-Tools + Platform-Tools'

$ndk = $env:NDK_HOME
if (-not $ndk) { $ndk = $env:ANDROID_NDK_ROOT }
$ndkOk = $ndk -and (Test-Path $ndk)
Need 'Android NDK（NDK_HOME 指向它）' $ndkOk `
    'SDK Manager → SDK Tools → 勾 NDK (Side by side)，然后设 NDK_HOME'

# 这一条最容易被忽略：zstd-sys 是 C 代码，交叉编译必须有 NDK 的 clang。
# 缺它的话，连 omy-core 都编不过，跟 Tauri 无关
$clangOk = $false
if ($ndkOk) {
    $clang = Join-Path $ndk 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe'
    $clangOk = Test-Path $clang
}
Need 'NDK 里的 clang（zstd-sys 编译必需）' $clangOk `
    '随 NDK 一起提供；已装 NDK 仍缺则说明 NDK 目录不完整'

$targets = rustup target list --installed 2>&1
$t1 = $targets -match 'aarch64-linux-android'
Need 'Rust 目标 aarch64-linux-android' ([bool]$t1) `
    'rustup target add aarch64-linux-android'

$t2 = $targets -match 'armv7-linux-androideabi'
Need 'Rust 目标 armv7-linux-androideabi' ([bool]$t2) `
    'rustup target add armv7-linux-androideabi'

$tauriOk = $false
$tv = cargo tauri --version 2>&1
if ($LASTEXITCODE -eq 0) { $tauriOk = $true }
Need 'tauri-cli（cargo tauri）' $tauriOk `
    'cargo install tauri-cli --version "^2"'

Write-Output ''
Write-Output "=== 就绪 $ok 项，缺 $miss 项 ==="

if ($miss -eq 0) {
    Write-Output ''
    Write-Output '全部就绪，可以执行：'
    Write-Output '  cd crates\omy-gui'
    Write-Output '  cargo tauri android init'
    Write-Output '  cargo tauri android dev      # 连真机或模拟器'
    Write-Output '  cargo tauri android build    # 出 APK'
} else {
    Write-Output ''
    Write-Output '补齐后重跑本脚本确认，再执行 cargo tauri android init。'
}

exit 0
