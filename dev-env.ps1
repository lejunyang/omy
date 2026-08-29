# omy 项目开发环境初始化
# 用途：确保 Rust 镜像环境变量在当前会话生效
# 使用：. .\dev-env.ps1     （注意前面有点和空格，表示在当前会话中执行）

$env:RUSTUP_DIST_SERVER = 'https://mirrors.tuna.tsinghua.edu.cn/rustup'
$env:RUSTUP_UPDATE_ROOT = 'https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup'
$env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = 'sparse'

Write-Host "omy 开发环境已就绪" -ForegroundColor Green
Write-Host ("  rustc  : " + (rustc --version)) -ForegroundColor Gray
Write-Host ("  cargo  : " + (cargo --version)) -ForegroundColor Gray
Write-Host ("  工具链源: " + $env:RUSTUP_DIST_SERVER) -ForegroundColor Gray
Write-Host ("  crates : rsproxy.cn (sparse)") -ForegroundColor Gray
