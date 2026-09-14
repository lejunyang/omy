#!/usr/bin/env bash
# FFmpeg 裁剪构建的组件配方。两个档位共用本文件，只由 $OMY_FF_PROFILE 切换差异。
#
# 为什么两档共用一份而不是各写一份：分档差异只有「要不要带专利编解码器」这
# 一件事，其余上百个 flag 完全相同。写成两份配方的必然结果是改了一边忘了另一
# 边——而这类错误编得过、跑得动，只在特定输入上才暴露。
#
# 用法：source 本文件后读 $FF_FLAGS 数组。需要先设好 SYSROOT / WEBP / ZLIB。
#
# 每一条 flag 的取舍都有实测依据，见 docs/research/13-ffmpeg-minimal-build.md
# 的 §4.2.1 与 §4.5。改动前请先读那两节，多数「看起来多余」的 flag 都是踩过坑
# 才加上的。

set -euo pipefail

: "${OMY_FF_PROFILE:?必须设为 royalty-free 或 full}"
: "${SYSROOT:?}"
: "${WEBP:?}"
: "${ZLIB:?}"

case "$OMY_FF_PROFILE" in
  royalty-free|full) ;;
  *) echo "未知档位: $OMY_FF_PROFILE（只接受 royalty-free / full）" >&2; exit 2 ;;
esac

# ---- 各档位的视频解码器 -------------------------------------------------
#
# 分档依据是专利风险，不是文件流行度：
#   免版税档  VP8 / VP9 / AV1 —— 均有明确的免版税授权承诺
#   全量档    再加 H.264 / HEVC —— 由 MPEG LA / Access Advance 等收取许可费
#
# mjpeg / png / webp / rawvideo 两档都要：前三个是图片格式（缩略图源），
# rawvideo 是 omy 图片路径的管道输入格式，都不涉及视频编码专利。
VIDEO_DEC_COMMON='vp8,vp9,av1,mjpeg,png,webp,rawvideo'
VIDEO_DEC_PATENTED='h264,hevc'

if [ "$OMY_FF_PROFILE" = full ]; then
  VIDEO_DEC="${VIDEO_DEC_COMMON},${VIDEO_DEC_PATENTED}"
  # parser/bsf 也要跟着分档：留着 h264 的 parser 却没有 decoder 没有意义，
  # 而 bsf 是 -c copy 正确产出 MP4 的前提（见下面 parser/bsf 那段注释）
  VIDEO_PARSER="vp8,vp9,av1,png,webp,h264,hevc"
  VIDEO_BSF="extract_extradata,h264_mp4toannexb,hevc_mp4toannexb"
else
  VIDEO_DEC="${VIDEO_DEC_COMMON}"
  VIDEO_PARSER="vp8,vp9,av1,png,webp"
  VIDEO_BSF="extract_extradata"
fi

# ---- 音频解码器（两档相同）--------------------------------------------
#
# AAC 与 MP3 严格说也有专利，但 MP3 的专利已于 2017 年全部到期；AAC 保留在
# 两档里是因为它是 MP4 的事实标准音轨，去掉会让绝大多数视频连时长都探测不出。
# 这个取舍写进文档 §2，若法务口径变化只需改这一行。
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
