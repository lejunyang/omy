param(
    [string]$Output = (Join-Path $PSScriptRoot 'build\omy-test-editor.apk')
)

$ErrorActionPreference = 'Stop'

# 工具版本由项目 osdk.toml 管理；脚本只消费 osdk 暴露的 shim 和共享 SDK，
# 不写 osdk 的 data/config 管控目录。
$sdkInfo = & osdk android sdk-root show
if ($LASTEXITCODE -ne 0) { throw '无法读取 osdk Android SDK 根目录' }
$first = @($sdkInfo)[0]
if ($first -notmatch '^sdk root:\s*(.+)$') { throw '无法解析 osdk Android SDK 根目录' }
$sdk = $Matches[1].Trim()
$androidJar = Join-Path $sdk 'platforms\android-36\android.jar'
if (-not (Test-Path -LiteralPath $androidJar -PathType Leaf)) {
    throw "找不到 Android 平台库：$androidJar"
}

$work = Join-Path $PSScriptRoot 'build\editor-work'
$classes = Join-Path $work 'classes'
$dex = Join-Path $work 'dex'
$unsigned = Join-Path $work 'unsigned.apk'
$aligned = Join-Path $work 'aligned.apk'
$jarFile = Join-Path $work 'editor.jar'
Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $classes, $dex, (Split-Path -Parent $Output) | Out-Null

& javac -encoding UTF-8 -source 8 -target 8 -classpath $androidJar -d $classes `
    (Join-Path $PSScriptRoot 'src\org\omy\testeditor\EditorActivity.java')
if ($LASTEXITCODE -ne 0) { throw '编译测试编辑器 Java 失败' }

& jar cf $jarFile -C $classes .
if ($LASTEXITCODE -ne 0) { throw '打包测试编辑器 class 失败' }
& d8 --lib $androidJar --min-api 24 --output $dex $jarFile
if ($LASTEXITCODE -ne 0) { throw '生成测试编辑器 DEX 失败' }

& aapt2 link -o $unsigned -I $androidJar --manifest (Join-Path $PSScriptRoot 'AndroidManifest.xml') `
    --min-sdk-version 24 --target-sdk-version 35
if ($LASTEXITCODE -ne 0) { throw '生成测试编辑器 APK 资源失败' }
& jar uf $unsigned -C $dex classes.dex
if ($LASTEXITCODE -ne 0) { throw '写入测试编辑器 DEX 失败' }
& zipalign -f 4 $unsigned $aligned
if ($LASTEXITCODE -ne 0) { throw '对齐测试编辑器 APK 失败' }

$keystore = Join-Path $env:USERPROFILE '.android\debug.keystore'
if (-not (Test-Path -LiteralPath $keystore -PathType Leaf)) {
    throw "找不到 Android 调试签名：$keystore"
}
& apksigner sign --ks $keystore --ks-key-alias androiddebugkey `
    --ks-pass pass:android --key-pass pass:android --out $Output $aligned
if ($LASTEXITCODE -ne 0) { throw '签名测试编辑器 APK 失败' }
& apksigner verify $Output
if ($LASTEXITCODE -ne 0) { throw '测试编辑器 APK 签名校验失败' }

Write-Output $Output
