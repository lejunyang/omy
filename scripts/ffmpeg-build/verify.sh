#!/usr/bin/env bash
# 校验 FFmpeg 产物：许可证边界、外部依赖、以及 omy 真实用到的每个组件。
#
# 为什么必须有这个脚本：本轮踩到的四个问题（rawvideo / fd 协议 /
# image_png_pipe / movtext）**全都是 configure 接受了参数但没启用，且不报错**
# ——configure 退出 0、make 退出 0、手工命令行还能跑通，只有 omy 自己的测试
# 才失败。所以「编过了」完全不能作为接受标准，必须逐项复查组件是否真的在。
#
# 用法: bash verify.sh <产物目录>

set -uo pipefail

OUT="${1:?用法: verify.sh <产物目录> [期望架构]}"
# 期望架构仅 macOS 用：第二参数（或 OMY_FF_ARCH）给出时，lipo 实际架构必须等于它；
# 不给则从产物目录名（out-arm64 / dist-ffmpeg-x86_64 之类）推断，仍推不出就只校验
# 「单一架构」，不做一致性子项。这里传值是为了避免 arm64/x86_64 产物错塞进对方包。
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# 产物名随平台：Windows 是 ffmpeg.exe，Linux 是 ffmpeg。按目录里实际存在的文件
# 选择，这样在任一平台都能校验同一份产物布局。
if [ -f "$OUT/ffmpeg.exe" ]; then EXE=.exe; else EXE=""; fi
FFMPEG="$OUT/ffmpeg${EXE}"
FFPROBE="$OUT/ffprobe${EXE}"
fail=0

# 平台与可移植工具差异：
#   stat：BSD/macOS 用 -f%z，GNU/Linux 用 -c %s。
#   外部依赖：Windows 查 objdump 的 DLL，Linux 查 ldd，macOS 查 otool -L。
case "$(uname -s)" in
  Darwin*) PLATFORM=macos ;;
  MINGW*|MSYS*|CYGWIN*) PLATFORM=windows ;;
  *) PLATFORM=linux ;;
esac
if stat -f%z "$0" >/dev/null 2>&1; then
  size_of() { stat -f%z "$1"; }
else
  size_of() { stat -c %s "$1"; }
fi

note() { printf '  %-46s %s\n' "$1" "$2"; }
bad()  { printf '  %-46s %s\n' "$1" "失败: $2"; fail=$((fail + 1)); }

for f in "$FFMPEG" "$FFPROBE"; do
  [ -f "$f" ] || { echo "缺少 $f" >&2; exit 1; }
done

echo "=== 校验 ${OUT}（${PLATFORM}）==="
echo
echo "--- 1. 体积 ---"
total=0
for f in "$FFMPEG" "$FFPROBE"; do
  sz="$(size_of "$f")"
  total=$((total + sz))
  printf '  %-14s %10d 字节 (%.2f MB)\n' "$(basename "$f")" "$sz" "$(echo "$sz" | awk '{printf "%.2f", $1/1048576}')"
done
printf '  %-14s %10d 字节 (%.2f MB)\n' '合计' "$total" "$(echo "$total" | awk '{printf "%.2f", $1/1048576}')"

echo
echo "--- 2. 许可证边界 ---"
# 一旦误开 gpl/nonfree/version3，整个分发前提就变了，而这在产物上看不出来
banner="$("$FFMPEG" -hide_banner -buildconf 2>&1)"
for flag in enable-gpl enable-nonfree enable-version3; do
  if grep -q -- "--$flag" <<<"$banner"; then
    bad "未启用 --$flag" "检测到该 flag"
  else
    note "未启用 --$flag" "ok"
  fi
done
if "$FFMPEG" -version 2>&1 | grep -qiE 'LGPL version 2\.1|--disable-gpl'; then
  note "许可证为 LGPL 2.1+" "ok"
else
  bad "许可证为 LGPL 2.1+" "既无 LGPL 声明也无 --disable-gpl"
fi

echo
echo "--- 2b. 随包分发的许可证正文与源码说明 ---"
# LGPL 强制要求随二进制提供许可证正文，以及一份说明如何获取对应源码的文字。
# 这两项过去靠构建脚本「尽量拷」，失败被 || true 吞掉，产物可能缺文件而构建仍绿。
# 现在缺失即硬失败：少了它们，分发在许可证层面就是不合法的。
LGPL="$OUT/COPYING.LGPLv2.1"
SOURCE_NOTE="$OUT/SOURCE.txt"
for f in "$LGPL" "$SOURCE_NOTE"; do
  if [ ! -s "$f" ]; then
    bad "$(basename "$f") 存在且非空" "缺失或为空"
  else
    note "$(basename "$f") 存在且非空" "ok（$(size_of "$f") 字节）"
  fi
done
# SOURCE.txt 还得真的对应这次构建：带上 VERSION 文件锁的版本号与源码校验和。
# 只写「基于 FFmpeg」没用——版本对不上，用户照它拿的源码编不出同一个二进制。
EXPECT_VER="$(sed -n 1p "$HERE/VERSION" 2>/dev/null || true)"
if [ -s "$SOURCE_NOTE" ]; then
  if [ -n "$EXPECT_VER" ] && ! grep -qF "$EXPECT_VER" "$SOURCE_NOTE"; then
    bad "SOURCE.txt 指向构建版本" "未找到 ${EXPECT_VER}"
  else
    note "SOURCE.txt 指向构建版本" "ok（${EXPECT_VER}）"
  fi
  if grep -qiE 'sha256|校验和|源码 SHA' "$SOURCE_NOTE"; then
    note "SOURCE.txt 含源码校验和" "ok"
  else
    bad "SOURCE.txt 含源码校验和" "未出现 SHA256/校验和"
  fi
fi

echo
echo "--- 3. 外部运行时依赖 ---"
if [ "$PLATFORM" = windows ]; then
  # mingw 构建有时会拖上 libgcc_s_seh-1.dll 之类，那样就不能只拷两个 exe。
  # 同理若 libwebp/zlib 没静态进去，会多出 libwebp.dll 甚至 VCRUNTIME140.dll。
  if command -v x86_64-w64-mingw32-objdump >/dev/null 2>&1; then
    ext="$(x86_64-w64-mingw32-objdump -p "$FFMPEG" \
          | sed -n 's/.*DLL Name: *//p' \
          | grep -Ei 'libgcc|libwinpthread|libstdc|msys|libwebp|vcruntime|msvcp|libz' || true)"
    if [ -n "$ext" ]; then
      bad "无外部运行时 DLL" "$(tr '\n' ' ' <<<"$ext")"
    else
      note "无外部运行时 DLL" "ok"
    fi
  else
    note "无外部运行时 DLL" "跳过（objdump 不可用）"
  fi
elif [ "$PLATFORM" = macos ]; then
  # macOS：libwebp / zlib 必须已静态嵌入，otool -L 里不应出现它们。
  # 系统库（libSystem.B.dylib 等）动态链接是预期的，两架构都如此。
  if command -v otool >/dev/null 2>&1; then
    ext="$(otool -L "$FFMPEG" \
          | grep -Ei 'libwebp|libz\.' || true)"
    if [ -n "$ext" ]; then
      bad "libwebp/zlib 已静态嵌入" "$(tr '\n' ' ' <<<"$ext")"
    else
      note "libwebp/zlib 已静态嵌入" "ok"
    fi
    # 架构要对：release 会把 arm64 产物塞进 arm64 tarball、x86_64 塞进 x86_64
    # tarball，混了就是换错目录，运行即崩。这一段从「打印一下」升级为硬失败：
    #   1) lipo 只能列出一个架构——出现两个就是 fat/universal，我们的配方只编单架构；
    #   2) 若能确定期望架构（第二参数 / OMY_FF_ARCH / 目录名推断），实际必须等于它，
    #      否则按错包处理。lipo 本身不可用也按失败算（fail-closed，宁可误拦不可漏过）。
    if command -v lipo >/dev/null 2>&1; then
      archs="$(lipo -archs "$FFMPEG" 2>/dev/null | tr '\n' ' ' | sed 's/[[:space:]]*$//')"
      want_arch="${2:-${OMY_FF_ARCH:-}}"
      if [ -z "$want_arch" ]; then
        case "$(basename "$OUT")" in
          *arm64*|*aarch64*) want_arch=arm64 ;;
          *x86_64*|*amd64*|*intel*) want_arch=x86_64 ;;
        esac
      fi
      case "$want_arch" in
        arm64|aarch64) want_arch=arm64 ;;
        x86_64|amd64)  want_arch=x86_64 ;;
        *)             want_arch="" ;;
      esac
      actual_first="$(printf '%s' "$archs" | awk '{print $1}')"
      case "$actual_first" in
        arm64|aarch64) actual_arch=arm64 ;;
        x86_64|amd64)  actual_arch=x86_64 ;;
        *)             actual_arch="" ;;
      esac
      n_arch="$(printf '%s' "$archs" | wc -w | tr -d ' ')"
      if [ "$n_arch" != "1" ]; then
        bad "产物为单一架构" "lipo 列出 ${n_arch} 个: ${archs:-空}"
      elif [ -z "$actual_arch" ]; then
        bad "产物架构可识别" "未知: ${archs:-空}"
      elif [ -n "$want_arch" ] && [ "$actual_arch" != "$want_arch" ]; then
        bad "产物架构与目标一致" "实际 ${actual_arch}，期望 ${want_arch}"
      elif [ -n "$want_arch" ]; then
        note "产物架构（lipo -archs）" "${archs}（期望 ${want_arch}）"
      else
        note "产物架构（lipo -archs）" "${archs}（未指定期望架构，仅校验单架构）"
      fi
    else
      bad "产物架构校验" "lipo 不可用，无法确认 arm64/x86_64 未混包"
    fi
  else
    note "libwebp/zlib 已静态嵌入" "跳过（otool 不可用）"
  fi
else
  # Linux：libwebp / zlib 必须已静态嵌入，ldd 里不应出现它们。
  # glibc / libgcc_s 等系统库动态链接是预期的（见 configure-flags.sh 的说明）。
  if command -v ldd >/dev/null 2>&1; then
    ext="$(ldd "$FFMPEG" \
          | awk '{print $1}' \
          | grep -Ei 'libwebp|libz\.so' || true)"
    if [ -n "$ext" ]; then
      bad "libwebp/zlib 已静态嵌入" "$(tr '\n' ' ' <<<"$ext")"
    else
      note "libwebp/zlib 已静态嵌入" "ok"
    fi
  else
    note "libwebp/zlib 已静态嵌入" "跳过（ldd 不可用）"
  fi
fi

echo
echo "--- 4. omy 真实用到的组件 ---"
have_demuxer() { "$FFMPEG" -hide_banner -demuxers 2>/dev/null | awk '{print $2}' | grep -qx "$1"; }
have_decoder() { "$FFMPEG" -hide_banner -decoders 2>/dev/null | awk '{print $2}' | grep -qx "$1"; }
have_encoder() { "$FFMPEG" -hide_banner -encoders 2>/dev/null | awk '{print $2}' | grep -qx "$1"; }
have_proto()   { "$FFMPEG" -hide_banner -protocols 2>/dev/null | grep -qx "  *$1" \
                 || "$FFMPEG" -hide_banner -protocols 2>/dev/null | tr -d ' ' | grep -qx "$1"; }
have_filter()  { "$FFMPEG" -hide_banner -filters 2>/dev/null | awk '{print $2}' | grep -qx "$1"; }

check() { # check <类型> <名字> <用在哪>
  local kind="$1" name="$2" why="$3"
  if "have_$kind" "$name"; then
    note "$kind $name" "ok"
  else
    bad "$kind $name" "缺失（${why}）"
  fi
}

# 这四项是本轮实际踩到的，缺任何一项 omy 的图片缩略图路径都会静默失效。
#
# 注意名字有两套：configure 的组件名与 CLI 列表里的显示名不一定相同，
#   configure --enable-demuxer=image_png_pipe  ->  CLI 显示 png_pipe
#   configure --enable-encoder=movtext         ->  CLI 显示 mov_text
# 这里查的是 CLI 名（也就是 omy 实际会传给 ffmpeg 的名字），
# 因为最终起作用的是它，而不是构建期怎么写。
check demuxer rawvideo  "thumbnail.rs 把 RGB 裸数据喂进管道"
check demuxer png_pipe  "ffprobe 从管道识别 PNG；缺了 is_still_image 判不出图片"
check proto   fd        "FFmpeg 7.x 把 -i - 解析成 fd:，只有 pipe 不够"
check encoder libwebp   "thumbnail.rs 的 -c:v libwebp"
check encoder mov_text  "remux.rs 的 -c:s mov_text"
check decoder png       "ffprobe 读 PNG 宽高，缺了返回 width=0"
check decoder rawvideo  "同 rawvideo demuxer"
check filter  scale     "缩略图缩放"
check filter  thumbnail "视频抽帧选帧"
check proto   pipe      "全程管道 IO"

echo
echo "--- 5. 视频解码器齐全 ---"
# 这段防的是「改配方时专利解码器意外掉出去」。
#
# 它必须留着：h264/hevc 掉了之后，configure 退出 0、make 退出 0、体积只小了
# 两三兆，产物看上去完全正常——直到用户打开一个 H.264 视频，发现探测得出时长
# 却抽不出缩略图（报 no decoder found for: h264）。这个症状既像功能坏了，
# 又不会有任何构建期信号，只有在这里逐项查才拦得住。
#
# h264/hevc 与 vp8/vp9/av1 现在是同一类要求：全都必须在。
for c in h264 hevc vp8 vp9 av1; do
  check decoder "$c" "omy 需要能为它抽缩略图"
done

echo
if [ "$fail" -eq 0 ]; then
  echo "全部通过。"
else
  echo "有 $fail 项未通过。" >&2
  exit 1
fi
