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

OUT="${1:?用法: verify.sh <产物目录>}"

# 产物名随平台：Windows 是 ffmpeg.exe，Linux 是 ffmpeg。按目录里实际存在的文件
# 选择，这样在任一平台都能校验同一份产物布局。
if [ -f "$OUT/ffmpeg.exe" ]; then EXE=.exe; else EXE=""; fi
FFMPEG="$OUT/ffmpeg${EXE}"
FFPROBE="$OUT/ffprobe${EXE}"
fail=0

note() { printf '  %-46s %s\n' "$1" "$2"; }
bad()  { printf '  %-46s %s\n' "$1" "失败: $2"; fail=$((fail + 1)); }

for f in "$FFMPEG" "$FFPROBE"; do
  [ -f "$f" ] || { echo "缺少 $f" >&2; exit 1; }
done

echo "=== 校验 $OUT ==="
echo
echo "--- 1. 体积 ---"
total=0
for f in "$FFMPEG" "$FFPROBE"; do
  sz="$(stat -c %s "$f")"
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
echo "--- 3. 外部运行时依赖 ---"
if [ "$EXE" = .exe ]; then
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
    bad "$kind $name" "缺失（$why）"
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
