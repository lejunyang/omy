Set-Location 'E:\Projects\omy'
git rm --cached 'spikes/gui-commit.ps1' --quiet
Remove-Item 'spikes\gui-commit.ps1' -Force -EA SilentlyContinue
Add-Content -Path '.gitignore' -Value '/spikes/gui-commit.ps1'
git add -A
git commit -q -m 'chore: 把一次性的 GUI 构建脚本移出版本库

verify-gui-*.ps1 与 probe-gui-vue.mjs 留下，它们是可复跑的验证；
gui-commit.ps1 只是这次提交用的胶水，没有复用价值。'
Write-Output '--- 现状 ---'
git log --oneline -2 | ForEach-Object { Write-Output ("  " + $_) }
git status --short | ForEach-Object { Write-Output ("  " + $_) }
Write-Output '  (工作区干净)'
