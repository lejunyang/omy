#!/usr/bin/env bash
# 按依赖顺序逐个发布 workspace 里对外公开的 crate。
#
# 为什么不用 `cargo publish --workspace` 一把梭：crates.io 对**新 crate**
# 的限流是「单账号 burst 5 个，之后每 10 分钟只准 1 个」
# （https://crates.io/docs/rate-limits）。首次发布有 6 个全新包，
# --workspace 会连续打出去，第 6 个必然吃 429 而失败；而前 5 个此时已经
# 发出去且删不掉，重跑又要处理「部分已发布」的状态。所以这里逐个发，
# 自己控制新 crate 之间的等待。
#
# 对已存在 crate 的**新版本**，限制是 burst 30 个、之后每分钟 1 个，
# 本 workspace 只有 6 个包，永远在 burst 内；它们之间的短等待只是为了让
# sparse 索引（CDN）传播，好让依赖它的下一个包 publish 时能解析到版本。
set -euo pipefail

# 顺序即依赖顺序：被依赖的先发。publish=false 的包（omy-gui / omy-remote /
# spikes）不在此列，`cargo publish -p` 也不会碰它们。
CRATES=(
  omy-core
  omy-config
  omy-media
  omy-net
  omy-secret
  omy-cli
)

# 与官方限流数字对齐，冷却时间故意略大于 10 分钟，贴着边界等容易碰上
# 服务端计时的误差。
readonly NEW_CRATE_BURST=5
readonly NEW_CRATE_COOLDOWN=610   # 秒，> 10 分钟
readonly INDEX_PROPAGATION_DELAY=30 # 秒，等 sparse 索引传播
readonly MAX_ATTEMPTS=3

api_http_code() {
  # crates.io 数据访问政策要求请求带可识别的 User-Agent。
  # 只取 HTTP 状态码：200 = 包已存在（发新版本），404 = 全新包。
  curl -s -o /dev/null -w '%{http_code}' \
    -H 'User-Agent: omy-release (https://github.com/lejunyang/omy)' \
    "https://crates.io/api/v1/crates/$1"
}

publish_one() {
  local crate="$1" attempt
  for ((attempt = 1; attempt <= MAX_ATTEMPTS; attempt++)); do
    if cargo publish -p "$crate"; then
      return 0
    fi
    # cargo 撞 429 时只是非零退出，没有专门的退出码可分辨。主动等待已经
    # 覆盖了正常路径；这里兜底：失败后等一个完整的新 crate 冷却窗口再试，
    # 无论失败原因是不是限流都无害（编译类错误重试也还是失败，三次后放弃）。
    if ((attempt < MAX_ATTEMPTS)); then
      echo "::warning::${crate} 发布未成功（第 ${attempt} 次），${NEW_CRATE_COOLDOWN}s 后重试（若为 429 需要等满限流窗口）"
      sleep "${NEW_CRATE_COOLDOWN}"
    fi
  done
  echo "::error::${crate} 连续 ${MAX_ATTEMPTS} 次发布失败，终止"
  return 1
}

main() {
  : "${CARGO_REGISTRY_TOKEN:?缺少 CARGO_REGISTRY_TOKEN}"

  local total="${#CRATES[@]}"
  local new_seen=0 i crate next code is_new

  for ((i = 0; i < total; i++)); do
    crate="${CRATES[$i]}"
    code="$(api_http_code "$crate")"
    if [[ "$code" == "200" ]]; then
      echo "==> ${crate}：已存在，发布新版本"
      is_new=0
    else
      # 404 是预期内的「全新包」；其它非 200（网络问题等）也按新包处理——
      # 那只会让等待变长，是偏安全的误判，绝不会因此少等而撞限流。
      echo "==> ${crate}：crates.io 状态码 ${code}，按首次发布处理"
      is_new=1
    fi

    publish_one "$crate"
    ((is_new)) && new_seen=$((new_seen + 1))

    # 最后一个发完不用等。
    ((i == total - 1)) && continue
    next="${CRATES[$((i + 1))]}"

    if ((new_seen >= NEW_CRATE_BURST)) && [[ "$(api_http_code "$next")" != "200" ]]; then
      # burst（5 个新 crate）已用完，且下一个仍是全新包：官方限制此后
      # 每 10 分钟才能再发 1 个新 crate。
      echo "::group::等待 ${NEW_CRATE_COOLDOWN}s（新 crate 限流冷却，之后再发 ${next}）"
      sleep "${NEW_CRATE_COOLDOWN}"
      echo "::endgroup::"
    else
      echo "等待 ${INDEX_PROPAGATION_DELAY}s 让 registry 索引传播后再发 ${next}"
      sleep "${INDEX_PROPAGATION_DELAY}"
    fi
  done

  echo "全部发布完成：${CRATES[*]}"
}

main "$@"
