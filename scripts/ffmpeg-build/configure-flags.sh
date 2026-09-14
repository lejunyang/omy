#!/usr/bin/env bash
# FFmpeg 裁剪构建的组件配方。
#
# 用法：source 本文件后读 $FF_FLAGS 数组。需要先设好 SYSROOT / WEBP / ZLIB。
#
# 每一条 flag 的取舍都有实测依据，见 docs/research/13-ffmpeg-minimal-build.md
# 的 §4.2.1 与 §4.5。改动前请先读那两节，多数「看起来多余」的 flag 都是踩过坑
# 才加上的。

set -euo pipefail

: "${SYSROOT:?}"
: "${WEBP:?}"
: "${ZLIB:?}"

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

FF_FLAGS=(
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
  --extra-cflags="-B${SYSROOT}/usr/lib -I${SYSROOT}/usr/include -I${WEBP}/include -I${ZLIB}/include -O2"
  --extra-ldflags="-B${SYSROOT}/usr/lib -L${WEBP}/lib -L${ZLIB}/lib -static"
  --pkg-config-flags=--static

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
