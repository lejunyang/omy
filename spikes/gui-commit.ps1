Set-Location 'E:\Projects\omy'
foreach ($f in 'gui-progress.ps1','gui-git.ps1','gui-install.ps1','gui-build.ps1','gui-cargo.ps1','gui-cleanup.ps1','ls-gui.ps1') {
    Remove-Item (Join-Path 'spikes' $f) -Force -EA SilentlyContinue
}
git add -A
git commit -q -F '.git/COMMIT_MSG_gui'
Remove-Item '.git/COMMIT_MSG_gui' -Force
Write-Output '--- 最近提交 ---'
git log --oneline -3 | ForEach-Object { Write-Output ("  " + $_) }
Write-Output ''
Write-Output '--- 本次变更 ---'
git show --stat --format='' HEAD | ForEach-Object { Write-Output ("  " + $_) }
