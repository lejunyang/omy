#!/usr/bin/env bash
# 在 Linux 上原生构建 omy 所需的最小 FFmpeg（含自建的静态 libwebp 与 zlib）。
#
# 前置条件：项目根目录的 osdk.toml 已提供 nasm（系统通常不自带）；
# gcc / make / cmake / pkg-config 用系统已有的即可，本脚本不调 osdk。
# 用法：
#   bash scripts/ffmpeg-build/build-linux.sh
#
# 产物只有一份，包含 omy 需要的全部解码能力（含 H.264 / HEVC），组件清单与
# Windows 版完全相同（见 configure-flags.sh）。曾经按专利风险分过两档，现已
# 取消，理由见 configure-flags.sh 顶部。

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${OMY_FF_WORK:-/tmp/omy-ffmpeg-build}"

FFMPEG_VER="$(sed -n 1p "$HERE/VERSION")"
FFMPEG_SHA256="$(sed -n 2p "$HERE/VERSION")"
WEBP_VER="$(sed -n 3p "$HERE/VERSION")"
WEBP_SHA256="$(sed -n 4p "$HERE/VERSION")"
ZLIB_VER="$(sed -n 5p "$HERE/VERSION")"
ZLIB_SHA256="$(sed -n 6p "$HERE/VERSION")"

SRC="$WORK/src"
OUT="${OMY_FF_OUT:-$WORK/out}"
export WEBP="$WORK/deps/webp"
export ZLIB="$WORK/deps/zlib"

mkdir -p "$SRC" "$OUT" "$WEBP" "$ZLIB"

# nasm：osdk 的 nasm shim 按「CWD 的 osdk 配置」选版本，本脚本若不从项目根
# 目录调起（CI / 自动化常见），shim 会报 no version selected，configure 因此
# 认为没有 nasm。这里解析出已安装的 nasm 实体路径，经 --x86asmexe 显式传给
# configure。调用方也可用 NASM=... 覆盖；项目根调起时 PATH 上的 shim 本就可用。
if [ -z "${NASM:-}" ]; then
  osdk_root="${OSDK_DATA_DIR:-$HOME/.local/share/osdk}"
  for c in "$osdk_root"/installs/conda/nasm/*/*/bin/nasm; do
    if [ -x "$c" ]; then NASM="$c"; break; fi
  done
fi
export NASM

CURL="$(command -v curl)"
[ -n "$CURL" ] || { echo "找不到可用的 curl" >&2; exit 1; }

# 校验 SHA256 而不是只看文件在不在：上游 tarball 被替换过的事情发生过，
# 而「体积对得上、内容被动过」是最难查的一类问题。
fetch() {
  local url="$1" file="$2" want="$3"
  if [ -f "$file" ]; then
    local got
    got="$(sha256sum "$file" | cut -d' ' -f1)"
    if [ "$got" = "$want" ]; then
      echo "  已有且校验通过: $(basename "$file")"
      return 0
    fi
    echo "  校验和不符，重新下载: $(basename "$file")" >&2
    rm -f "$file"
  fi
  echo "  下载 $(basename "$file") ..."
  # --retry-all-errors：实测 zlib.net 在本网路会先传几十 KB 再 connection reset，
  # 默认只对瞬态 HTTP 码重试，reset 直接失败退出；加上后可自动重试。
  "$CURL" -fsSL --retry 5 --retry-all-errors -o "$file" "$url"
  local got
  got="$(sha256sum "$file" | cut -d' ' -f1)"
  if [ "$got" != "$want" ]; then
    echo "SHA256 不符：$(basename "$file")" >&2
    echo "  期望 $want" >&2
    echo "  实际 $got" >&2
    exit 1
  fi
}

echo "=== 1/5 获取源码 ==="
fetch "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VER}.tar.xz" \
      "$SRC/ffmpeg-${FFMPEG_VER}.tar.xz" "$FFMPEG_SHA256"
fetch "https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-${WEBP_VER}.tar.gz" \
      "$SRC/libwebp-${WEBP_VER}.tar.gz" "$WEBP_SHA256"
fetch "https://zlib.net/fossils/zlib-${ZLIB_VER}.tar.gz" \
      "$SRC/zlib-${ZLIB_VER}.tar.gz" "$ZLIB_SHA256"

cd "$SRC"
[ -d "ffmpeg-${FFMPEG_VER}" ] || tar -xf "ffmpeg-${FFMPEG_VER}.tar.xz"
[ -d "libwebp-${WEBP_VER}" ]  || tar -xf "libwebp-${WEBP_VER}.tar.gz"
[ -d "zlib-${ZLIB_VER}" ]     || tar -xf "zlib-${ZLIB_VER}.tar.gz"

echo
echo "=== 2/5 构建静态 zlib ==="
# 不用发行版的 zlib：不能保证是静态库，且版本/path 不受控。自编一份与
# Windows 流程同版本，产物确定只有 .a。
rm -rf "$WORK/bld/zlib"
# 不用 Ninja（系统未必装）：Unix Makefiles 是系统自带 make 就能用的生成器。
cmake -S "$SRC/zlib-${ZLIB_VER}" -B "$WORK/bld/zlib" -G "Unix Makefiles" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$ZLIB" -DBUILD_SHARED_LIBS=OFF >/dev/null
cmake --build "$WORK/bld/zlib" --target install >/dev/null
# Windows 上 cmake 产物叫 libzlibstatic.a；Linux 上通常直接是 libz.a。
# 哪个名字都补出 FFmpeg 按 -lz 查找的 libz.a，避免依赖「这次 cmake 怎么命名」。
if [ ! -f "$ZLIB/lib/libz.a" ]; then
  cp -f "$ZLIB"/lib/libz*.a "$ZLIB/lib/libz.a"
fi
# zlib 的 CMakeLists 无视 BUILD_SHARED_LIBS，始终同时装 .a 与 .so。链接器在
# -L 目录里优先选 .so，那会让产物动态拖 libz —— 删掉共享库，强制只用静态。
rm -f "$ZLIB"/lib/libz.so "$ZLIB"/lib/libz.so.*
echo "  libz.a $(stat -c %s "$ZLIB/lib/libz.a") 字节"

echo
echo "=== 3/5 构建静态 libwebp ==="
# 关掉一切附带的命令行工具与 extras：我们只要被 FFmpeg 链接的 libwebp.a。
rm -rf "$WORK/bld/webp"
cmake -S "$SRC/libwebp-${WEBP_VER}" -B "$WORK/bld/webp" -G "Unix Makefiles" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$WEBP" -DBUILD_SHARED_LIBS=OFF \
  -DCMAKE_INSTALL_LIBDIR=lib \
  -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
  -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF \
  -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF \
  -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_ANIM_UTILS=OFF \
  -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=OFF >/dev/null
cmake --build "$WORK/bld/webp" --target install >/dev/null
echo "  libwebp.a $(stat -c %s "$WEBP/lib/libwebp.a") 字节"

echo
echo "=== 4/5 配置并构建 FFmpeg ==="
export FF_TARGET=linux
# shellcheck source=./configure-flags.sh
source "$HERE/configure-flags.sh"

BLD="$WORK/bld/ffmpeg"
rm -rf "$BLD"
mkdir -p "$BLD"
cd "$BLD"

# .pc 的位置随平台而变：libwebp 走 GNUInstallDirs（部分发行版默认 lib64），
# zlib 装在 share/pkgconfig。各候选位置都给上，配置时已用 -DCMAKE_INSTALL_LIBDIR
# 归一到 lib，lib64 只是兜底。
export PKG_CONFIG_PATH="$WEBP/lib/pkgconfig:$WEBP/lib64/pkgconfig:$ZLIB/lib/pkgconfig:$ZLIB/share/pkgconfig"

# < /dev/null：不给 configure 任何标准输入，避免它在某些探测分支里等输入
"$SRC/ffmpeg-${FFMPEG_VER}/configure" --prefix="$OUT" "${FF_FLAGS[@]}" < /dev/null

make -j"$(nproc)"

# make 产出的 ffmpeg/ffprobe 已经是 strip 后的（未 strip 的是 ffmpeg_g/ffprobe_g）。
cp -f ffmpeg ffprobe "$OUT/"

echo
echo "=== 5/5 产物 ==="
for f in ffmpeg ffprobe; do
  printf '  %-12s %10d 字节\n' "$f" "$(stat -c %s "$OUT/$f")"
done

# 许可证文件必须随产物分发：LGPL 要求提供许可证正文，
# 且要说明如何获取对应源码（SOURCE.txt）。这里不能 2>/dev/null || true：
# 拷失败要让构建立刻报错，而不是等 verify.sh 以「缺许可证」告终。
cp -f "$SRC/ffmpeg-${FFMPEG_VER}/COPYING.LGPLv2.1" "$OUT/COPYING.LGPLv2.1"
cat > "$OUT/SOURCE.txt" <<EOF
本目录中的 ffmpeg / ffprobe 由 omy 项目自行编译。

FFmpeg 版本: ${FFMPEG_VER}
源码地址:    https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VER}.tar.xz
源码 SHA256: ${FFMPEG_SHA256}
配方:        scripts/ffmpeg-build/configure-flags.sh（见 omy 仓库）
许可证:      LGPL v2.1 或更高版本（见 COPYING.LGPLv2.1）

同时静态链接了：
  libwebp ${WEBP_VER}  https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-${WEBP_VER}.tar.gz
  zlib    ${ZLIB_VER}  https://zlib.net/fossils/zlib-${ZLIB_VER}.tar.gz

按 LGPL 的要求，你可以用同一份配方重新构建并替换这两个可执行文件。
EOF

echo
echo "产物目录: $OUT"
echo "下一步: bash $HERE/verify.sh $OUT"
