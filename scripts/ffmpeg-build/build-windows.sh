#!/usr/bin/env bash
# 在 Windows 上交叉编译 omy 所需的最小 FFmpeg（含自建的静态 libwebp 与 zlib）。
#
# 前置条件：项目根目录的 osdk.toml 已 trust，工具链由它提供（不需要 MSYS2）。
# 用法：
#   OMY_FF_PROFILE=royalty-free bash scripts/ffmpeg-build/build-windows.sh
#   OMY_FF_PROFILE=full         bash scripts/ffmpeg-build/build-windows.sh
#
# 注意：本脚本内**不调用 osdk**。嵌套的 osdk 会尝试交互提示并卡在等 stdin 上
# （实测挂了 18 分钟只烧掉 4.8 CPU 秒，看起来像死锁）。所以工具路径必须由外层
# 的 prepare-toolchain.ps1 算好后通过环境变量传入。

set -euo pipefail

: "${OMY_FF_PROFILE:?必须设为 royalty-free 或 full}"
: "${SYSROOT:?必须由外层传入（见脚本头部注释，本脚本不能自己调 osdk）}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${OMY_FF_WORK:-/tmp/omy-ffmpeg-build}"

FFMPEG_VER="$(sed -n 1p "$HERE/VERSION")"
FFMPEG_SHA256="$(sed -n 2p "$HERE/VERSION")"
WEBP_VER="$(sed -n 3p "$HERE/VERSION")"
WEBP_SHA256="$(sed -n 4p "$HERE/VERSION")"
ZLIB_VER="$(sed -n 5p "$HERE/VERSION")"
ZLIB_SHA256="$(sed -n 6p "$HERE/VERSION")"

SRC="$WORK/src"
OUT="$WORK/out/$OMY_FF_PROFILE"
export WEBP="$WORK/deps/webp"
export ZLIB="$WORK/deps/zlib"

mkdir -p "$SRC" "$OUT" "$WEBP" "$ZLIB"

# 下载器：优先用 Windows 自带的 curl，而不是 msys 那个。
#
# conda 的 m2-base 里 curl 确实有，但它附带的 /usr/ssl/certs/ca-bundle.crt
# 是**0 字节**，于是每次下载都死在
#   curl: (77) error setting certificate file: /usr/ssl/certs/ca-bundle.crt
# 报错说的是「设置证书文件失败」，看起来像本机装漏了什么，实际是上游包就这样，
# 重装、换镜像都没用。Windows 自带的 curl 走 Schannel 用系统证书store，没有
# 这个问题（实测 8.21.0 可用）。
#
# 不用 -k 跳过校验：那等于把供应链校验关掉换取一次下载成功。
pick_curl() {
  if [ -x /c/Windows/System32/curl.exe ]; then
    echo /c/Windows/System32/curl.exe
  else
    command -v curl
  fi
}
CURL="$(pick_curl)"
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
  "$CURL" -fsSL --retry 3 -o "$file" "$url"
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

# 解压必须在 POSIX 路径下进行：msys 的 tar 会把 "C:\..." 里的 C: 当成远程
# 主机名，报 "Cannot connect to C: resolve failed"。
#
# 用 bsdtar 而不是 tar：m2-base 的 GNU tar 解 .xz 时要外部调 xz，而 m2-base
# 里没有 xz，报 "xz: Cannot exec"。想补 conda:m2-xz 又会撞 msys2-conda-epoch
# 版本冲突装不上。bsdtar 自带 lzma 支持，一个命令解决三种格式。
cd "$SRC"
[ -d "ffmpeg-${FFMPEG_VER}" ] || bsdtar -xf "ffmpeg-${FFMPEG_VER}.tar.xz"
[ -d "libwebp-${WEBP_VER}" ]  || bsdtar -xf "libwebp-${WEBP_VER}.tar.gz"
[ -d "zlib-${ZLIB_VER}" ]     || bsdtar -xf "zlib-${ZLIB_VER}.tar.gz"

CFLAGS_COMMON="-B${SYSROOT}/usr/lib -I${SYSROOT}/usr/include -O2"

echo
echo "=== 2/5 构建静态 zlib ==="
# 不用 conda 的 zlib：win-64 包是 MSVC 构建，产物会依赖 VCRUNTIME140.dll，
# 等于要随 omy 一起分发 MSVC 运行时。
rm -rf "$WORK/bld/zlib"
cmake -S "$SRC/zlib-${ZLIB_VER}" -B "$WORK/bld/zlib" -G Ninja \
  -DCMAKE_SYSTEM_NAME=Windows -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$ZLIB" -DBUILD_SHARED_LIBS=OFF \
  -DCMAKE_C_COMPILER=x86_64-w64-mingw32-gcc \
  -DCMAKE_C_FLAGS="$CFLAGS_COMMON" \
  -DCMAKE_RC_COMPILER=x86_64-w64-mingw32-windres >/dev/null
cmake --build "$WORK/bld/zlib" --target install >/dev/null
# cmake 装出来的名字是 libzlibstatic.a，而 FFmpeg 按 -lz 找 libz.a
cp -f "$ZLIB/lib/libzlibstatic.a" "$ZLIB/lib/libz.a"
echo "  libz.a $(stat -c %s "$ZLIB/lib/libz.a") 字节"

echo
echo "=== 3/5 构建静态 libwebp ==="
# 同样不用 conda 的 libwebp：只提供 MSVC 的 .lib，链出来的 exe 依赖
# libwebp.dll，而该 DLL 又依赖 VCRUNTIME140.dll。
rm -rf "$WORK/bld/webp"
cmake -S "$SRC/libwebp-${WEBP_VER}" -B "$WORK/bld/webp" -G Ninja \
  -DCMAKE_SYSTEM_NAME=Windows -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$WEBP" -DBUILD_SHARED_LIBS=OFF \
  -DCMAKE_C_COMPILER=x86_64-w64-mingw32-gcc \
  -DCMAKE_C_FLAGS="$CFLAGS_COMMON" \
  -DCMAKE_RC_COMPILER=x86_64-w64-mingw32-windres \
  -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
  -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF \
  -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF \
  -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_ANIM_UTILS=OFF \
  -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=OFF >/dev/null
cmake --build "$WORK/bld/webp" --target install >/dev/null
echo "  libwebp.a $(stat -c %s "$WEBP/lib/libwebp.a") 字节"

echo
echo "=== 4/5 配置并构建 FFmpeg（档位: $OMY_FF_PROFILE）==="
# shellcheck source=./configure-flags.sh
source "$HERE/configure-flags.sh"

BLD="$WORK/bld/ffmpeg-$OMY_FF_PROFILE"
rm -rf "$BLD"
mkdir -p "$BLD"
cd "$BLD"

export PKG_CONFIG_PATH="$WEBP/lib/pkgconfig:$ZLIB/share/pkgconfig"

# < /dev/null：不给 configure 任何标准输入，避免它在某些探测分支里等输入
"$SRC/ffmpeg-${FFMPEG_VER}/configure" --prefix="$OUT" "${FF_FLAGS[@]}" < /dev/null

make -j"$(nproc)"

cp -f ffmpeg.exe ffprobe.exe "$OUT/"

echo
echo "=== 5/5 产物 ==="
for f in ffmpeg.exe ffprobe.exe; do
  printf '  %-12s %10d 字节\n' "$f" "$(stat -c %s "$OUT/$f")"
done

# 许可证文件必须随产物分发：LGPL 要求提供许可证正文，
# 且要说明如何获取对应源码（SOURCE.txt）。
cp -f "$SRC/ffmpeg-${FFMPEG_VER}/COPYING.LGPLv2.1" "$OUT/" 2>/dev/null || true
cat > "$OUT/SOURCE.txt" <<EOF
本目录中的 ffmpeg.exe / ffprobe.exe 由 omy 项目自行编译。

FFmpeg 版本: ${FFMPEG_VER}
源码地址:    https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VER}.tar.xz
源码 SHA256: ${FFMPEG_SHA256}
构建档位:    ${OMY_FF_PROFILE}
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
