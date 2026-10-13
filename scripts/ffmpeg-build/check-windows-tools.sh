#!/usr/bin/env bash
# 必须在项目树外检查：只在仓库根执行 --version 会漏掉切目录后 shim 丢失版本的回归。
set -euo pipefail
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

cmake --version
ninja --version
nasm -v
bsdtar --version
x86_64-w64-mingw32-gcc -dumpversion
x86_64-w64-mingw32-nm --version

# FFmpeg 会直接执行带 shebang 的临时脚本，--version 和 make 的简单命令测不到它。
type -a sh
ls -ld /bin /bin/sh /bin/sh.exe /usr/bin/sh /usr/bin/sh.exe || true
sh -c 'cmake --version && ninja --version'
printf '#!/bin/sh\n' > check-shell.sh
chmod +x check-shell.sh
./check-shell.sh

# make 启动的子 shell 也要能找到工具，否则 configure 通过后仍会在编译阶段失败。
printf 'check:\n\tcmake --version && ninja --version && pkg-config --version\n' > Makefile
make check
