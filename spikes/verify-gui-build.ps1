$dist = 'E:\Projects\omy\crates\omy-gui\dist'
$js = Join-Path $dist 'app.js'
$src = [System.IO.File]::ReadAllText($js)

Write-Output '=== 1. 产物里不能有 eval / new Function（CSP 会拦） ==='
$bad = @('eval(', 'new Function(', 'Function("', "Function('")
$found = $false
foreach ($p in $bad) {
    $n = ([regex]::Matches($src, [regex]::Escape($p))).Count
    if ($n -gt 0) { Write-Output "  FAIL  出现 $p x$n"; $found = $true }
}
if (-not $found) { Write-Output '  PASS  未出现 eval / new Function' }

Write-Output ''
Write-Output '=== 2. 模板里的 i18n.t 是否被正确编译（命名空间导入可用？） ==='
# 编译后 i18n 的成员访问会保留成某种形式；找 locked_name 这个只在模板里出现的键
if ($src.Contains('file.locked_name')) {
    Write-Output '  PASS  模板中的 i18n 调用已编入产物（找到 file.locked_name）'
} else {
    Write-Output '  FAIL  找不到 file.locked_name，模板可能没编译进去'
}

Write-Output ''
Write-Output '=== 3. 不能打进完整版 Vue（含模板编译器） ==='
# 完整版会包含编译器特有的字符串
if ($src.Contains('Template compilation') -or $src.Contains('compile-time')) {
    Write-Output '  FAIL  疑似打进了模板编译器'
} else {
    Write-Output '  PASS  未发现模板编译器痕迹'
}

Write-Output ''
Write-Output '=== 4. index.html 引用与 CSP 兼容（无内联脚本） ==='
$html = [System.IO.File]::ReadAllText((Join-Path $dist 'index.html'))
Write-Output $html
if ($html -match '<script(?![^>]*\ssrc=)[^>]*>[\s\S]*?\S[\s\S]*?</script>') {
    Write-Output '  FAIL  存在内联脚本'
} else {
    Write-Output '  PASS  无内联脚本'
}

Write-Output ''
Write-Output '=== 5. locales 是否随产物复制 ==='
$loc = Join-Path $dist 'locales'
if (Test-Path $loc) {
    Get-ChildItem -File $loc | ForEach-Object { Write-Output ("  PASS  {0}" -f $_.Name) }
} else {
    Write-Output '  FAIL  locales 缺失，界面会全是键名'
}

Write-Output ''
Write-Output '=== 6. 产物清单 ==='
Get-ChildItem -Recurse -File $dist | ForEach-Object {
    Write-Output ("  {0,9}  {1}" -f $_.Length, $_.FullName.Replace("$dist\", ''))
}
