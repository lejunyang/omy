#!/usr/bin/env bash
# 在 macOS 上构建 omy 所需的最小 FFmpeg（含自建的静态 libwebp 与 zlib）。
#
# 同时支持两种架构：
#   ARCH=arm64    Apple Silicon（在 arm64 机器上原生编）
#   ARCH=x86_64  Intel（在 arm64 机器上交叉编；macOS 两架构共用 SDK，
#                只靠 -arch 区分，不需要额外 sysroot）
# 不设 ARCH 时按本机架构。release 流水线在 macos-latest（arm64）上各编一份，
# 分别放进 aarch64 与 x86_64 两份 tarball。
#
# 前置条件：Xcode Command Line Tools（提供 clang 与 macOS SDK），以及
# pkg-config、cmake、nasm（x86_64 的 SIMD 汇编用；arm64 用不到）。
# CI 上用 `brew install pkg-config nasm cmake` 装齐。
#
# 用法：
#   bash scripts/ffmpeg-build/build-macos.sh            # 本机架构
#   ARCH=x86_64 bash scripts/ffmpeg-build/build-macos.sh
#
# 产物与 Windows / Linux 完全同配方（见 configure-flags.sh），只含 omy 需要的
# 全部解码能力（含 H.264 / HEVC），不分档，理由见 configure-flags.sh 顶部。

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${OMY_FF_WORK:-/tmp/omy-ffmpeg-build}"

FFMPEG_VER="$(sed -n 1p "$HERE/VERSION")"
FFMPEG_SHA256="$(sed -n 2p "$HERE/VERSION")"
WEBP_VER="$(sed -n 3p "$HERE/VERSION")"
WEBP_SHA256="$(sed -n 4p "$HERE/VERSION")"
ZLIB_VER="$(sed -n 5p "$HERE/VERSION")"
ZLIB_SHA256="$(sed -n 6p "$HERE/VERSION")"

# 目标架构：默认本机，可被 ARCH 覆盖。统一成 FFmpeg configure 认的名字。
NATIVE="$(uname -m)"
ARCH="${ARCH:-$NATIVE}"
case "$ARCH" in
  arm64|aarch64) FF_ARCH=arm64 ;;
  x86_64|amd64)  FF_ARCH=x86_64 ;;
  *) echo "未知 ARCH: ${ARCH}（只支持 arm64 / x86_64）" >&2; exit 1 ;;
esac
export FF_ARCH

SRC="$WORK/src"
OUT="${OMY_FF_OUT:-$WORK/out}"
export WEBP="$WORK/deps/webp-$FF_ARCH"
export ZLIB="$WORK/deps/zlib-$FF_ARCH"

mkdir -p "$SRC" "$OUT" "$WEBP" "$ZLIB"

# clang 必须解析成 Xcode 的真实路径，而不是 PATH 上的 `clang`：
# 开发机 PATH 上的 clang 常是 osdk shim，它在本机是坏的（把 --version 当成
# 无输入文件）。同时导出 SDKROOT：直接路径调 clang 时它不知道 SDK 在哪，
# 会在链接期报 library 'System' not found。
export CC="$(xcrun --find clang)"
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
echo "架构         = ${FF_ARCH}（本机 ${NATIVE}）"
echo "编译器       = $CC"
echo "SDKROOT      = $SDKROOT"

for tool in cmake pkg-config make; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "找不到 ${tool}。macOS 前置：xcode-select --install，并" >&2
    echo "brew install pkg-config nasm cmake" >&2
    exit 1
  }
done

JOBS="$(sysctl -n hw.ncpu)"

CURL="$(command -v curl)"
[ -n "$CURL" ] || { echo "找不到可用的 curl" >&2; exit 1; }

# 校验 SHA256 而不是只看文件在不在：上游 tarball 被替换过的事情发生过，
# 而「体积对得上、内容被动过」是最难查的一类问题。
fetch() {
  local url="$1" file="$2" want="$3"
  if [ -f "$file" ]; then
    local got
    got="$(shasum -a 256 "$file" | cut -d' ' -f1)"
    if [ "$got" = "$want" ]; then
      echo "  已有且校验通过: $(basename "$file")"
      return 0
    fi
    echo "  校验和不符，重新下载: $(basename "$file")" >&2
    rm -f "$file"
  fi
  echo "  下载 $(basename "$file") ..."
  "$CURL" -fsSL --retry 5 --retry-all-errors -o "$file" "$url"
  local got
  got="$(shasum -a 256 "$file" | cut -d' ' -f1)"
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

# CMAKE_OSX_ARCHITECTURES 是 Apple 官方的「同 SDK 换架构」方式：CMake 自动把
# -arch 加到编译与链接，arm64/x86_64 都用同一份系统头，不需要工具链文件。
CMAKE_ARCH="-DCMAKE_OSX_ARCHITECTURES=$FF_ARCH"

echo
echo "=== 2/5 构建静态 zlib（${FF_ARCH}）==="
rm -rf "$WORK/bld/$FF_ARCH/zlib"
cmake -S "$SRC/zlib-${ZLIB_VER}" -B "$WORK/bld/$FF_ARCH/zlib" -G "Unix Makefiles" \
  $CMAKE_ARCH \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_COMPILER="$CC" \
  -DCMAKE_INSTALL_PREFIX="$ZLIB" -DBUILD_SHARED_LIBS=OFF >/dev/null
cmake --build "$WORK/bld/$FF_ARCH/zlib" --target install >/dev/null
# 兜底命名：FFmpeg 按 -lz 找 libz.a。
if [ ! -f "$ZLIB/lib/libz.a" ]; then
  cp -f "$ZLIB"/lib/libz*.a "$ZLIB/lib/libz.a"
fi
# 删掉动态库，强制 FFmpeg 只链静态（否则 -L 优先选 .dylib，产物会拖 libz）。
rm -f "$ZLIB"/lib/libz.dylib "$ZLIB"/lib/libz.*.dylib
echo "  libz.a $(stat -f%z "$ZLIB/lib/libz.a") 字节"

echo
echo "=== 3/5 构建静态 libwebp（${FF_ARCH}）==="
rm -rf "$WORK/bld/$FF_ARCH/webp"
cmake -S "$SRC/libwebp-${WEBP_VER}" -B "$WORK/bld/$FF_ARCH/webp" -G "Unix Makefiles" \
  $CMAKE_ARCH \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_COMPILER="$CC" \
  -DCMAKE_INSTALL_PREFIX="$WEBP" -DBUILD_SHARED_LIBS=OFF \
  -DCMAKE_INSTALL_LIBDIR=lib \
  -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
  -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF \
  -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF \
  -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_ANIM_UTILS=OFF \
  -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=OFF >/dev/null
cmake --build "$WORK/bld/$FF_ARCH/webp" --target install >/dev/null
rm -f "$WEBP"/lib/libwebp.dylib "$WEBP"/lib/libwebp.*.dylib
echo "  libwebp.a $(stat -f%z "$WEBP/lib/libwebp.a") 字节"

echo
echo "=== 4/5 配置并构建 FFmpeg（${FF_ARCH}）==="
export FF_TARGET=macos
# shellcheck source=./configure-flags.sh
source "$HERE/configure-flags.sh"

BLD="$WORK/bld/$FF_ARCH/ffmpeg"
rm -rf "$BLD"
mkdir -p "$BLD"
cd "$BLD"

export PKG_CONFIG_PATH="$WEBP/lib/pkgconfig:$ZLIB/lib/pkgconfig:$ZLIB/share/pkgconfig"

# < /dev/null：不给 configure 任何标准输入，避免它在某些探测分支里等输入。
"$SRC/ffmpeg-${FFMPEG_VER}/configure" --prefix="$OUT" "${FF_FLAGS[@]}" < /dev/null

make -j"$JOBS"

cp -f ffmpeg ffprobe "$OUT/"

echo
echo "=== 5/5 产物（${FF_ARCH}）==="
for f in ffmpeg ffprobe; do
  printf '  %-12s %10d 字节\n' "$f" "$(stat -f%z "$OUT/$f")"
done

# 许可证文件必须随产物分发：LGPL 要求提供许可证正文，
# 且要说明如何获取对应源码（SOURCE.txt）。
cp -f "$SRC/ffmpeg-${FFMPEG_VER}/COPYING.LGPLv2.1" "$OUT/" 2>/dev/null || true
cat > "$OUT/SOURCE.txt" <<EOF
本目录中的 ffmpeg / ffprobe 由 omy 项目自行编译（macOS ${FF_ARCH}）。

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
