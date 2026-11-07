#!/usr/bin/env bash
# 按依赖顺序逐个发布 workspace 里对外公开的 crate。
#
# 为什么不用 `cargo publish --workspace` 一把梭：crates.io 对**新 crate**
# 的限流是「单账号 burst 5 个，之后每 10 分钟只准 1 个」
# （https://crates.io/docs/rate-limits）。首次发布有 8 个全新包，
# --workspace 会连续打出去，第 6 个起必然吃 429 而失败；而前 5 个此时已经
# 发出去且删不掉，重跑又要处理「部分已发布」的状态。所以这里逐个发，
# 自己控制新 crate 之间的等待。
#
# 对已存在 crate 的**新版本**，限制是 burst 30 个、之后每分钟 1 个，
# 本 workspace 只有 8 个包，永远在 burst 内；它们之间的短等待只是为了让
# sparse 索引（CDN）传播，好让依赖它的下一个包 publish 时能解析到版本。
#
# # 可重跑
#
# 中途失败（限流超出预期、网络、runner 被回收）后直接重跑整条流水线即可：
# 某个包的**这个版本**已经在 crates.io 上，就跳过它，不再上传——否则 cargo
# 会报「already exists」，被当成失败反复重试。
set -euo pipefail

# 顺序即依赖顺序：被依赖的先发。
#   omy-core ← omy-config ← omy-secret ← omy-remote ← omy-cli
#   omy-media、omy-net 只依赖 core / config
#   omy-password-manager 是独立的库（GUI 用），放最后不阻塞 CLI
# omy-gui 与 spikes 标了 publish = false，不在此列：omy-gui 的 build.rs 要在
# 安装时跑 pnpm 构建前端，`cargo install` 的环境里没有这些。
CRATES=(
  omy-core
  omy-config
  omy-secret
  omy-media
  omy-net
  omy-remote
  omy-cli
  omy-password-manager
)

# 与官方限流数字对齐，冷却时间故意略大于 10 分钟，贴着边界等容易碰上
# 服务端计时的误差。
readonly NEW_CRATE_BURST=5
readonly NEW_CRATE_COOLDOWN=610   # 秒，> 10 分钟
readonly INDEX_PROPAGATION_DELAY=30 # 秒，等 sparse 索引传播
readonly RETRY_DELAY=60           # 秒，非限流失败（网络抖动等）的重试间隔
readonly MAX_ATTEMPTS=3
readonly UA='omy-release (https://github.com/lejunyang/omy)'

# workspace 统一版本，取自根 Cargo.toml 的 [workspace.package]。
VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
readonly VERSION

api_http_code() {
  # crates.io 数据访问政策要求请求带可识别的 User-Agent。
  # 只取 HTTP 状态码：200 = 存在，404 = 不存在。
  curl -s -o /dev/null -w '%{http_code}' -H "User-Agent: ${UA}" "https://crates.io/api/v1/$1"
}

publish_one() {
  local crate="$1" attempt log
  log="$(mktemp)"
  for ((attempt = 1; attempt <= MAX_ATTEMPTS; attempt++)); do
    if cargo publish -p "$crate" 2>&1 | tee "$log"; then
      rm -f "$log"
      return 0
    fi
    # 上一次其实已经传上去了（例如响应超时但服务端已收下），再传只会报
    # 已存在。以 registry 为准，不以 cargo 的退出码为准。
    if grep -qiE 'already (exists|uploaded)' "$log"; then
      echo "${crate}@${VERSION} 已在 crates.io 上，视为成功"
      rm -f "$log"
      return 0
    fi
    if ((attempt == MAX_ATTEMPTS)); then
      break
    fi
    # 撞限流要等满一个冷却窗口才有意义；其它失败（网络、索引尚未传播）
    # 只需短暂等待。编译类错误重试也还是失败，三次后放弃。
    if grep -qiE '429|too many requests|rate.?limit|published too many' "$log"; then
      echo "::warning::${crate} 被 crates.io 限流（第 ${attempt} 次），${NEW_CRATE_COOLDOWN}s 后重试"
      sleep "${NEW_CRATE_COOLDOWN}"
    else
      echo "::warning::${crate} 发布未成功（第 ${attempt} 次），${RETRY_DELAY}s 后重试"
      sleep "${RETRY_DELAY}"
    fi
  done
  rm -f "$log"
  echo "::error::${crate} 连续 ${MAX_ATTEMPTS} 次发布失败，终止。已发布的包不受影响，修复后重跑流水线会跳过它们。"
  return 1
}

main() {
  : "${CARGO_REGISTRY_TOKEN:?缺少 CARGO_REGISTRY_TOKEN}"
  [[ -n "$VERSION" ]] || { echo "::error::读不到 workspace 版本"; exit 1; }

  local total="${#CRATES[@]}"
  local new_seen=0 uploaded=0 i crate code is_new

  for ((i = 0; i < total; i++)); do
    crate="${CRATES[$i]}"

    if [[ "$(api_http_code "crates/${crate}/${VERSION}")" == "200" ]]; then
      echo "==> ${crate}@${VERSION}：已发布过，跳过"
      continue
    fi

    code="$(api_http_code "crates/${crate}")"
    if [[ "$code" == "200" ]]; then
      echo "==> ${crate}：已存在，发布新版本 ${VERSION}"
      is_new=0
    else
      # 404 是预期内的「全新包」；其它非 200（网络问题等）也按新包处理——
      # 那只会让等待变长，是偏安全的误判，绝不会因此少等而撞限流。
      echo "==> ${crate}：crates.io 状态码 ${code}，按首次发布处理"
      is_new=1
    fi

    # 等待放在**发之前**、按「这一个是不是新包」决定：burst 用完之后，
    # 每个新包发出前都要等一个冷却窗口。放在发之后再去猜下一个包的状态，
    # 遇到中间夹着已发布（跳过）的包时会算错。
    if ((uploaded > 0)); then
      if ((is_new)) && ((new_seen >= NEW_CRATE_BURST)); then
        echo "::group::等待 ${NEW_CRATE_COOLDOWN}s（新 crate 限流：burst ${NEW_CRATE_BURST} 个已用完，之后每 10 分钟 1 个）"
        sleep "${NEW_CRATE_COOLDOWN}"
        echo "::endgroup::"
      else
        echo "等待 ${INDEX_PROPAGATION_DELAY}s 让 registry 索引传播后再发 ${crate}"
        sleep "${INDEX_PROPAGATION_DELAY}"
      fi
    fi

    publish_one "$crate"
    uploaded=$((uploaded + 1))
    ((is_new)) && new_seen=$((new_seen + 1))
  done

  echo "全部完成（本次上传 ${uploaded} 个）：${CRATES[*]} @ ${VERSION}"
}

main "$@"
