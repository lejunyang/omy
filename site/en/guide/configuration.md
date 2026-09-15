---
title: Configuration file
---

# Configuration file

Turn frequently used flags into defaults so you stop repeating them.

## Location

omy **prefers to keep its configuration next to the program itself**, falling back to the system directory only when that location is not writable.

| Situation | Path |
|---|---|
| Program directory writable (portable) | `omy-data/config.toml` beside the executable |
| Windows (fallback) | `%APPDATA%\omy\config.toml` |
| Linux (fallback) | `~/.config/omy/config.toml` |
| macOS (fallback) | `~/Library/Application Support/omy/config.toml` |
| Android / iOS | The app's private data directory |

::: tip Why next to the program
omy often travels on an external drive or USB stick together with the encrypted files it manages. Configuration scattered across system directories is lost the moment you move to another machine; kept beside the program, copying that one folder takes every setting and the cache with it.

Installed into a read-only location such as `C:\Program Files` or `/usr/bin`, it falls back to the system directory automatically. The check actually writes a temporary file rather than inspecting permission bits — read-only mounts and Windows ACLs cannot be judged from permission bits alone, and getting it wrong means settings disappear silently.
:::

The file **does not exist by default**, in which case built-in defaults apply throughout. `omy doctor` prints the path actually in use; the GUI shows it under Settings › About.

Use `--config` to point somewhere else temporarily:

```bash
omy --config ./ci-config.toml encrypt file.mp4
```

::: tip Explicit paths behave differently from the default one
A file given via `--config` that cannot be read is an **error**; a missing file at the default path silently falls back to defaults. The distinction is deliberate — silently ignoring a file you named explicitly would let you believe your configuration took effect.
:::

## The GUI shares this same file

The GUI's Settings screen writes exactly this file: command line and graphical interface read the same configuration with the same defaults.

Two things worth knowing:

- **Saving preserves keys omy does not recognise.** Switching between an older and a newer version will not make them wipe each other's settings.
- **Saving loses comments.** Any comments you wrote by hand are gone after the GUI saves once.

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
# auto | dark | light
theme = "dark"
# grid | list
view = "list"

[security]
# Lock after this many idle seconds; 0 means never
auto_lock_secs = 600
# Lock immediately when the app is backgrounded
lock_on_background = true

[remote]
# Ciphertext cache limit in bytes; 0 means unlimited
cache_limit = 2147483648
scan_concurrency = 8
```

::: tip security and remote currently only affect the GUI
The command line has no use for auto-lock or a remote cache, but both read the same file, so these keys are preserved and never reported as unknown under the CLI.
:::

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
| `ui.theme` | `"auto"` | Interface theme; `auto` follows the system (GUI only) |
| `ui.view` | `"grid"` | Default view, `grid` or `list` (GUI only) |
| `security.auto_lock_secs` | `0` | Lock after this many idle seconds; `0` never (GUI only) |
| `security.lock_on_background` | `true` | Lock immediately when backgrounded (GUI only) |
| `remote.cache_limit` | `2147483648` | Ciphertext cache limit, 2 GiB; `0` unlimited (GUI only) |
| `remote.scan_concurrency` | `8` | Concurrent requests when scanning a remote location (GUI only) |

::: warning Think before setting original_action to trash or delete
Both values make the original disappear from its location after encryption. Putting it in the configuration file means **this happens on every encryption from now on**. Confirmation is still required (unless `--yes`), but configuration plus `--yes` in a script easily becomes silent deletion.
:::

## Misspelled keys are warned about and ignored

```toml
[defaults]
kdf_profil = "mobile"    # missing an e
```

```
警告: 配置中有无法识别的项 `defaults.kdf_profil`，已忽略
```

The command runs as usual and that one key falls back to its default. **The warning is the point** — silently ignoring a typo would let you believe your setting applied while defaults were used all along.

::: tip Why not reject the file outright
Earlier versions refused the whole file on an unknown key. But the GUI writes this same file, so the moment a newer version adds a key, an older version could no longer read it at all — switching between the two once would wipe every setting. Warning and ignoring keeps typos visible without letting cross-version reads and writes destroy each other.
:::

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
