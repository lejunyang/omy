---
title: 退出码
---

# 退出码

脚本可以据此区分「密码错」和「文件坏」——这是图形界面无法提供的精度。

| 码 | 含义 | 典型触发 |
|---|---|---|
| 0 | 成功 | — |
| 1 | 一般错误 | 文件不存在、读写失败 |
| 2 | 参数错误 | 拼错选项、用了 `--password` |
| 3 | 密码错误 / 无匹配 slot | `decrypt` / `cat` 密码不对 |
| 4 | 文件损坏或被篡改 | AEAD 认证失败、内容哈希不匹配 |
| 5 | 格式版本不支持 | 用旧版 omy 打开新版格式 |
| 6 | 缺少分片 | 分片不全且未加 `--ignore-missing-shards` |
| 7 | 权限不足 | 无权读写目标路径 |
| 8 | 用户取消 | 交互确认时选了否 |

## 实测行为

以下是实际运行的结果，注意 `verify` 与 `decrypt` 在密码错误时返回**不同的码**：

| 命令 | 情况 | 退出码 |
|---|---|---|
| `omy decrypt --password-file wrong.txt f.omy` | 密码错 | **3** |
| `omy cat --password-file wrong.txt f.omy` | 密码错 | **3** |
| `omy verify --password-file wrong.txt f.omy` | 密码错 | **4** |
| `omy info nosuch.omy` | 文件不存在 | 1 |
| `omy encrypt --password x f.txt` | 用了明文密码参数 | 2 |
| `omy verify --password-file right.txt f.omy` | 正常 | 0 |

::: warning verify 密码错返回 4 而不是 3
`verify` 的语义是「这个文件完好吗」。它试图解密并比对内容哈希，密码不对时解不出正确内容，报告的是「校验失败」（4），并在警告里说明真实原因是没有匹配的密钥槽。

所以**用 `verify` 的退出码判断密码对不对是不可靠的**。要区分这两种情况，用 `decrypt --verify-only`，它在密码错时返回 3。
:::

## 远程与虚拟命令的退出码

`remote` 下的命令复用这套码，但有自己的细分：

| 情况 | 退出码 | 说明 |
|---|---|---|
| `remote` / `remote telegram` / `remote virtual` 用法错 | 2 | clap 报 |
| 远程一般失败（连不上、读写失败、`telegram check` 不通） | 1 | 与「用法错 2」可据此区分 |
| 加密 Telegram 位置密码错 | **3** | `TG_PLACE_WRONG_PASSWORD` |
| `remote virtual` 密码错 | **3** | `VIRTUAL_WRONG_PASSWORD` |
| `remote virtual place remove` 等交互确认选否 | 8 | 与文件命令一致 |
| `remote virtual` 其余失败 | 1 | `VIRTUAL_*` 结构化码见下 |

`remote virtual` 的错误是结构化的：`--json` 的 `error.code` 形如 `VIRTUAL_LOCKED`（加密位置没给密码）、`VIRTUAL_WRONG_PASSWORD`（密码错 = 3）、`VIRTUAL_NOT_ENCRYPTED`、`VIRTUAL_NO_SUCH_PLACE` 等，脚本应匹配它而不是中文文案。

建连一个远程位置（`ls` / `upload` / `download` / `copy` / `decrypt` / `cache pin` 等）失败时，`--json` 的 `error.code` 给一组 `TG_*` 结构化码：

| `error.code` | 退出码 | 含义 |
|---|---|---|
| `TG_PLACE_WRONG_PASSWORD` | **3** | 给过位置密码但解不开加密的 Telegram 位置 |
| `TG_PLACE_PASSWORD_REQUIRED` | 1 | 加密位置本机自动解锁失败，又不在 TTY、也没给密码通道（脚本请用 `--password-*`） |
| `TG_PLACE_LOCKED` | 1 | 位置已加密但没解开（没给密码），提示用密码通道 |
| `TG_NO_SESSION` | 1 | 该位置没有可用的 Telegram 登录态，先 `telegram login` |
| `TG_SESSION_EXPIRED` | 1 | 登录态已失效（unauthorized），需重新登录 |
| `TG_CONNECT_NO_PROXY` | 1 | 直连 Telegram 失败且未配置代理 |
| `TG_SESSION_UNREADABLE` | 1 | 登录态文件读不出来 |
| `TG_CONNECT_FAILED` | 1 | 其它建连失败 |

WebDAV 位置不会产生这些码：它不需要现场位置密码。

Telegram 位置类**离线**操作（`encrypt` / `unlock` / `lock` / `decrypt` / `targets` / `forward` / `search`）的错误退出码恒为 1，靠消息里带方括号的稳定前缀区分：`[tg_unlock_wrong]` 密码错、`[tg_no_protector]` 无凭据库拒绝落明文、`[tg_not_encrypted]` 位置没加密、`[tg_no_session]` 还没登录态、`[tg_cross_chat]` 转发条目不在同一对话。脚本匹配前缀，不要匹配退出码。它与上面的 `TG_*` 结构化码是两套：前者是离线变换登录态信封时的人类可读前缀，后者是真正建连时进 `--json` 的机器可读码。

## 在脚本里用

```bash
omy decrypt --password-stdin archive.omy < pw.txt
case $? in
  0) echo "成功" ;;
  3) echo "密码不对" ;;
  4) echo "文件损坏或被篡改" ;;
  6) echo "分片不全" ;;
  *) echo "其他错误" ;;
esac
```

PowerShell：

```powershell
& omy decrypt --password-file pw.txt archive.omy
switch ($LASTEXITCODE) {
    0 { '成功' }
    3 { '密码不对' }
    4 { '文件损坏或被篡改' }
    default { "其他错误（$LASTEXITCODE）" }
}
```

## 配合 --json

`--json` 会把结构化的错误信息打到标准输出：

```json
{
  "error": {
    "code": "GENERAL_ERROR",
    "exit_code": 1,
    "message": "读取 note.omy 失败"
  }
}
```

::: tip code 恒为英文常量
`error.code` 不随界面语言变化（`WRONG_PASSWORD`、`GENERAL_ERROR` 等），而 `message` 是本地化的人类可读文本。

**脚本应该匹配 `code` 或退出码，不要匹配 `message`**——后者会随语言设置和版本变化。
:::

`info --json` 的字段是稳定契约的一部分，可以直接喂给 `jq`：

```bash
omy --json info file.omy | jq -r '.cipher, .chunk_size, .plaintext_size'
# chacha20-poly1305
# 262144
# 9670
```
