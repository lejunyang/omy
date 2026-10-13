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
sh -c 'cmake --version && ninja --version'
cat > check-configure.sh <<'EOF'
#!/bin/sh
set -eu
probe=$(mktemp -d "${TMPDIR:-/tmp}/ffconf.XXXXXXXX")
trap 'rm -rf "$probe"' EXIT
(set -C; exec > "$probe/test.sh")
printf '#!/bin/sh\n' > "$probe/test.sh"
chmod +x "$probe/test.sh"
"$probe/test.sh"
EOF
bash ./check-configure.sh < /dev/null

# make 启动的子 shell 也要能找到工具，否则 configure 通过后仍会在编译阶段失败。
printf 'check:\n\tcmake --version && ninja --version && pkg-config --version\n' > Makefile
make check
