#!/usr/bin/env bash
# FFmpeg 裁剪构建的组件配方。
#
# 用法：source 本文件后读 $FF_FLAGS 数组。
#   - Windows（交叉编译）：先设好 SYSROOT / WEBP / ZLIB，FF_TARGET=windows。
#   - Linux（原生构建）：先设好 WEBP / ZLIB，FF_TARGET=linux。
#   - macOS（原生或交叉）：先设好 WEBP / ZLIB / FF_ARCH，FF_TARGET=macos。
#   未显式给 FF_TARGET 时，从 SYSROOT 是否存在推断（兼容旧调用方式）。
#
# 每一条 flag 的取舍都有实测依据，见 docs/research/13-ffmpeg-minimal-build.md
# 的 §4.2.1 与 §4.5。改动前请先读那两节，多数「看起来多余」的 flag 都是踩过坑
# 才加上的。

set -euo pipefail

: "${WEBP:?}"
: "${ZLIB:?}"

FF_TARGET="${FF_TARGET:-}"
if [ -z "$FF_TARGET" ]; then
  if [ -n "${SYSROOT:-}" ]; then FF_TARGET=windows; else FF_TARGET=linux; fi
fi

# ---- 视频解码器 ---------------------------------------------------------
#
# 全部能力一次给齐，不分档。曾经按专利风险分过「免版税档 / 全量档」两份产物，
# 后来取消：H.264 覆盖了用户手上绝大多数视频，缺了它「探测得出时长、就是抽不
# 出缩略图」，而这个症状看起来完全就是功能坏了。专利立场改为在 README 与文档
# 站里声明，而不是靠砍能力来表达。
#
# mjpeg / png / webp 是图片格式（缩略图源），rawvideo 是 omy 图片路径的管道
# 输入格式。
VIDEO_DEC='vp8,vp9,av1,h264,hevc,mjpeg,png,webp,rawvideo'

# parser 与 decoder 要对应：留着 h264 的 parser 却没有 decoder 没有意义，
# 反过来缺了 parser/bsf 则会产出「能生成但播不了」的 MP4（见下面那段注释）。
VIDEO_PARSER='vp8,vp9,av1,png,webp,h264,hevc'
VIDEO_BSF='extract_extradata,h264_mp4toannexb,hevc_mp4toannexb'

# ---- 音频解码器 ---------------------------------------------------------
#
# AAC 与 MP3 严格说也有专利，但 MP3 的专利已于 2017 年全部到期；AAC 是 MP4 的
# 事实标准音轨，去掉会让绝大多数视频连时长都探测不出。
AUDIO_DEC='aac,mp3,opus,vorbis,flac,pcm_s16le'

# 两个平台共用的配方：从零开始只加需要的组件，许可证边界保持 LGPL-2.1+。
FF_FLAGS=(
  # 从零开始只加需要的，而不是从全集里减。
  --disable-everything
  --disable-doc
  --disable-shared
  --enable-static
  --enable-small
  --disable-network
  --disable-autodetect

  # 许可证边界：这三个一旦开启就不再是 LGPL-2.1+，verify.sh 会复查。
  --disable-gpl
  --disable-nonfree
  --disable-version3

  # 外部库。libwebp 编缩略图；zlib 供 PNG 解码器（ffprobe 读 PNG 宽高要用，
  # 缺了返回 width=0 height=0，omy 因此判定不出静态图片）。
  --enable-libwebp
  --enable-zlib

  --enable-decoder="${VIDEO_DEC}"
  --enable-decoder="${AUDIO_DEC}"
  # 字幕解码器。配套的编码器名是 movtext（无下划线），CLI 上却写
  # -c:s mov_text —— 按 CLI 名写进 configure 不报错也不启用。
  --enable-decoder=subrip,ass,webvtt,movtext

  --enable-encoder=libwebp,movtext

  # demuxer。pipe 类的真实名字带 image_ 前缀，写成 png_pipe 既不报错也不
  # 启用，结果 ffprobe 无法从管道识别 PNG —— 而 omy 只走管道探测。
  --enable-demuxer=mov,matroska,avi,mpegts,flv,image2,image2pipe
  --enable-demuxer=image_png_pipe,image_jpeg_pipe,image_webp_pipe
  --enable-demuxer=image_bmp_pipe,image_gif_pipe
  --enable-demuxer=mjpeg,webp,wav,mp3,flac,ogg,aac,srt,ass,webvtt,rawvideo

  --enable-muxer=mov,mp4,matroska,webp,image2,rawvideo

  # parser 与 bsf 必须显式开。只开 demuxer/muxer 时，-c copy 会因为拿不到
  # extradata 而产出「能生成但播不了」的 MP4 —— 这类问题不报错，只有真正
  # 播放才发现，remux.rs 的 looks_like_mp4 也拦不住。
  --enable-parser="${VIDEO_PARSER},aac,mpegaudio,flac,opus,vorbis"
  --enable-bsf="${VIDEO_BSF},aac_adtstoasc"

  # 协议。FFmpeg 7.x 把 -i - 解析成 fd:，只开 pipe 会报 Protocol not found。
  --enable-protocol=file,pipe,fd

  --enable-filter=scale,format,null,anull,copy,thumbnail,select,fps,transpose,crop
  --enable-swscale
  --enable-avfilter
)

# ---- 平台相关：编译器与链接参数 ----------------------------------------
case "$FF_TARGET" in
  windows)
    : "${SYSROOT:?Windows 交叉编译必须由外层传入 SYSROOT}"
    FF_FLAGS+=(
      # 交叉编译工具链。
      # configure 不读环境变量 CC，必须用 --cc= 显式传；binutils 只有带前缀的
      # 名字，缺 nm 时 configure 只是静默降级不报错，所以逐个显式指定。
      --cc=x86_64-w64-mingw32-gcc
      --nm=x86_64-w64-mingw32-nm
      --ar=x86_64-w64-mingw32-ar
      --ranlib=x86_64-w64-mingw32-ranlib
      --strip=x86_64-w64-mingw32-strip
      --windres=x86_64-w64-mingw32-windres
      --target-os=mingw32
      --arch=x86_64

      # CRT 在 <sysroot>/usr/lib 而非 <sysroot>/lib，gcc 默认搜不到。
      # 实测 --sysroot= 无效，必须用 -B 指出来，否则报 cannot find crt2.o。
      # -static：产物不拖带 mingw 运行时 DLL（verify.sh 会复查）。
      --extra-cflags="-B${SYSROOT}/usr/lib -I${SYSROOT}/usr/include -I${WEBP}/include -I${ZLIB}/include -O2"
      --extra-ldflags="-B${SYSROOT}/usr/lib -L${WEBP}/lib -L${ZLIB}/lib -static"
      --pkg-config-flags=--static
    )
    ;;
  linux)
    # 原生构建：不显式传 --cc，configure 自己找到系统 gcc。
    # nasm：PATH 上的 shim 可用时自动找到；build-linux.sh 解析出实体路径时
    # 经 --x86asmexe 显式传入，避免依赖 CWD 的 shim 选版本逻辑。
    if [ -n "${NASM:-}" ] && [ -x "$NASM" ]; then
      FF_FLAGS+=(--x86asmexe="$NASM")
    fi
    FF_FLAGS+=(
      #
      # 不加 -static：glibc 全静态有 NSS/getaddrinfo 等已知坑，而且 omy 只需要
      # libwebp / zlib 静态，它们由 -L 指向自编的 .a、配合 pkg-config --static
      # 嵌入二进制；glibc 仍动态链接（verify.sh 用 ldd 复查没有动态 libwebp/z）。
      --extra-cflags="-I${WEBP}/include -I${ZLIB}/include -O2"
      --extra-ldflags="-L${WEBP}/lib -L${ZLIB}/lib"
      --pkg-config-flags=--static
    )
    ;;
  macos)
    # FF_ARCH 由 build-macos.sh 传入：arm64（Apple Silicon）或 x86_64（Intel）。
    # macOS 两架构共用同一份 SDK，交叉编译只靠 -arch，不需要额外 sysroot：
    # 在 arm64 机器上编 x86_64 与原生编 arm64 用的是同一套系统头与库。
    #
    # --cc 走 build-macos.sh 解析出的真实 clang 路径（CC 环境变量），而不是
    # 裸 `clang`：开发机 PATH 上的 clang 可能是 osdk shim，它在本机是坏的。
    : "${FF_ARCH:?macOS 构建必须由外层传入 FF_ARCH（arm64 或 x86_64）}"
    FF_FLAGS+=(
      --target-os=darwin
      --arch="$FF_ARCH"
      --cc="${CC:-clang}"
      --extra-cflags="-arch ${FF_ARCH} -I${WEBP}/include -I${ZLIB}/include -O2"
      --extra-ldflags="-arch ${FF_ARCH} -L${WEBP}/lib -L${ZLIB}/lib"
      --pkg-config-flags=--static
    )
    # 交叉编译（目标架构 ≠ 本机架构）时必须显式 --enable-cross-compile：
    # configure 编完测试程序后默认会**运行**它来验证链接器，而在 arm64 机器上
    # 跑 x86_64 二进制会报 "Bad CPU type in executable"，configure 据此误判
    # "C compiler test failed"。开了交叉开关它就跳过运行测试程序这一步。
    case "$(uname -m)" in
      arm64|aarch64) host_arch=arm64 ;;
      x86_64|amd64)  host_arch=x86_64 ;;
      *)             host_arch=unknown ;;
    esac
    if [ "$FF_ARCH" != "$host_arch" ]; then
      FF_FLAGS+=(--enable-cross-compile)
    fi
    ;;
  *)
    echo "未知 FF_TARGET: ${FF_TARGET}（只支持 windows / linux / macos）" >&2
    exit 1
    ;;
esac
