#!/usr/bin/env bash
# 校验 FFmpeg 产物：许可证边界、外部依赖、以及 omy 真实用到的每个组件。
#
# 为什么必须有这个脚本：本轮踩到的四个问题（rawvideo / fd 协议 /
# image_png_pipe / movtext）**全都是 configure 接受了参数但没启用，且不报错**
# ——configure 退出 0、make 退出 0、手工命令行还能跑通，只有 omy 自己的测试
# 才失败。所以「编过了」完全不能作为接受标准，必须逐项复查组件是否真的在。
#
# 用法: bash verify.sh <产物目录> [档位]
#   档位省略时从目录名推断（out/royalty-free -> royalty-free）

set -uo pipefail

OUT="${1:?用法: verify.sh <产物目录> [royalty-free|full]}"
PROFILE="${2:-$(basename "$OUT")}"

FFMPEG="$OUT/ffmpeg.exe"
FFPROBE="$OUT/ffprobe.exe"
fail=0

note() { printf '  %-46s %s\n' "$1" "$2"; }
bad()  { printf '  %-46s %s\n' "$1" "失败: $2"; fail=$((fail + 1)); }

for f in "$FFMPEG" "$FFPROBE"; do
  [ -f "$f" ] || { echo "缺少 $f" >&2; exit 1; }
done

echo "=== 档位: $PROFILE ==="
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
echo "--- 5. 档位专属：专利编解码器 ---"
# 这段是分档的核心保障。免版税档一旦不小心带进 H.264，整个专利论证就失效，
# 而这种错误肉眼和体积都看不出来。
for c in h264 hevc; do
  if have_decoder "$c"; then
    if [ "$PROFILE" = full ]; then
      note "decoder $c 存在" "ok（全量档应有）"
    else
      bad "decoder $c 不应存在" "免版税档不得包含专利编解码器"
    fi
  else
    if [ "$PROFILE" = full ]; then
      bad "decoder $c 存在" "全量档缺失"
    else
      note "decoder $c 不存在" "ok（免版税档应无）"
    fi
  fi
done
# 两档都必须有的免版税视频解码器
for c in vp8 vp9 av1; do
  check decoder "$c" "免版税视频格式，两档都要"
done

echo
if [ "$fail" -eq 0 ]; then
  echo "全部通过。"
else
  echo "有 $fail 项未通过。" >&2
  exit 1
fi
