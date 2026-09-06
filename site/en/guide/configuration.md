---
title: Configuration file
---

# Configuration file

Turn frequently used flags into defaults so you stop repeating them.

## Location

| Platform | Path |
|---|---|
| Windows | `%APPDATA%\omy\config.toml` |
| Linux | `~/.config/omy/config.toml` |
| macOS | `~/Library/Application Support/omy/config.toml` |

The file **does not exist by default**, in which case built-in defaults apply throughout. `omy doctor` prints the path actually in use.

Use `--config` to point somewhere else temporarily:

```bash
omy --config ./ci-config.toml encrypt file.mp4
```

::: tip Explicit paths behave differently from the default one
A file given via `--config` that cannot be read is an **error**; a missing file at the default path silently falls back to defaults. The distinction is deliberate — silently ignoring a file you named explicitly would let you believe your configuration took effect.
:::

## Precedence

Command line flags > environment variables > configuration file > built-in defaults.

## A complete example

Every key below is real and every value valid:

```toml
[defaults]
# mobile | interactive | moderate | sensitive
kdf_profile = "moderate"
# xchacha20 | aes256gcm
cipher = "aes256gcm"
chunk_size = "4M"
# encrypt | keep-ext | plain
name_mode = "keep-ext"
# keep | trash | delete
original_action = "trash"

[compress]
enabled = true
level = 12          # zstd 1-19

[scan]
paths = ["~/Documents", "~/Videos"]
max_depth = 5

[serve]
device_name = "Study desktop"
default_expire = "24h"

[ui]
# auto | zh-CN | en
language = "en"
```

## Defaults for each key

| Key | Default | Meaning |
|---|---|---|
| `defaults.kdf_profile` | `"interactive"` | KDF profile |
| `defaults.cipher` | `"xchacha20"` | AEAD cipher |
| `defaults.chunk_size` | `"256K"` | Chunk size |
| `defaults.name_mode` | `"encrypt"` | Filename handling |
| `defaults.original_action` | `"keep"` | What happens to the original after encryption |
| `compress.enabled` | `false` | Compress by default |
| `compress.level` | `3` | zstd level |
| `scan.paths` | `[]` | Default paths for `scan` |
| `scan.max_depth` | `8` | Maximum recursion depth |
| `serve.device_name` | none | LAN advertised name |
| `serve.default_expire` | none | Default authorization lifetime |
| `ui.language` | `"auto"` | Interface language; `auto` follows the system locale |

::: warning Think before setting original_action to trash or delete
Both values make the original disappear from its location after encryption. Putting it in the configuration file means **this happens on every encryption from now on**. Confirmation is still required (unless `--yes`), but configuration plus `--yes` in a script easily becomes silent deletion.
:::

## Misspelled keys are rejected

Parsing is strict and unknown fields fail outright:

```toml
[defaults]
kdf_profil = "mobile"    # missing an e
```

```
错误: 读取配置失败: 解析配置文件 ... 失败
```

Deliberately so — silently ignoring a typo would let you believe your setting applied while defaults were used all along.

## Language

```toml
[ui]
language = "en"
```

Supports `zh-CN` and `en`; `auto` follows the system locale. `omy doctor` shows the effective language and the detected locale:

```
· 界面语言    zh-CN（系统 locale: zh-CN）
```

::: tip JSON output is language-independent
The `error.code` field in `--json` output is always an English constant (such as `WRONG_PASSWORD`), regardless of interface language, while `message` is localized human-readable text.

**Scripts should match `code` or the exit code, never `message`** — the latter changes with language settings and versions.
:::
