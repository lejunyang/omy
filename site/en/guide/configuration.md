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

Once the portable root is chosen, the rest of the local state lives under that same `omy-data/` folder, so copying the whole folder is a complete migration:

| State | Portable path | System fallback |
|---|---|---|
| Config file | `omy-data/config.toml` | `%APPDATA%\omy\config.toml`, etc. |
| Remote cache (temporary blocks + complete permanent files) | `omy-data/cache/` | `omy/` under the system cache dir |
| Logs | `omy-data/logs/` | `omy/logs/` under the system data dir |
| Device store (local identity + paired devices, holds private keys) | `omy-data/devices.omy` | `%APPDATA%\omy\devices.omy`, etc. |

The device store path can also be overridden explicitly via the `OMY_DEVICE_STORE` environment variable (pointing at a specific file); it takes precedence over the portable root — e.g. to keep your identity on a USB stick for multi-identity use. A blank value is treated as unset. When `OMY_DEVICE_STORE` is set, the program uses the path you name and does **not** perform the automatic migration below.

::: tip On upgrade, the old device store is migrated once automatically
The portable location was made the default later. Older builds kept device identity in the system config dir; after upgrading to portable mode, only when the **portable `devices.omy` does not yet exist but the legacy system one does** does the program copy it once into the portable location (write a temp file then rename; on failure it keeps using the legacy path this run and retries next launch). The legacy store is **never deleted**, and concurrent launches never overwrite each other. So your pairing identity travels with `omy-data/` instead of upgrading into a fresh blank identity.
:::

The file **does not exist by default**, in which case built-in defaults apply throughout. `omy doctor` prints the path actually in use; the GUI shows it under Settings › About.

Use `--config` to point somewhere else temporarily:

```bash
omy --config ./ci-config.toml encrypt file.mp4
```

::: tip Explicit paths behave differently from the default one
A file given via `--config` that cannot be read is an **error**; a missing file at the default path silently falls back to defaults. The distinction is deliberate — silently ignoring a file you named explicitly would let you believe your configuration took effect.
:::

## Logs

The GUI writes actions, backend calls and errors to a rolling log to help with troubleshooting.

| Situation | Path |
|---|---|
| Portable | `omy-data/logs/omy.log` beside the executable |
| Fallback | `omy/logs/omy.log` under the system data directory |
| Android / iOS | No file log is written |

The full log path is shown under Settings › About, with a button that opens the folder in your file manager.

A few things worth knowing:

- Each log file rolls over at 5 MiB, keeping at most four history files (`omy.log.1` through `omy.log.4`); older ones are deleted automatically. Disk usage is therefore bounded at roughly 25 MiB.
- The log **never** contains passwords, verification codes, two-step passwords, phone numbers, login credentials or decrypted file contents. Backend calls record only the method name, result and duration; file and chat identifiers are stored as an irreversible short hash, enough to tell whether two entries refer to the same item but not to recover the original path.
- When the log directory is not writable (for example on a read-only install), no file is written and the program runs normally.

## The GUI shares this same file

The GUI's Settings screen writes exactly this file: command line and graphical interface read the same configuration with the same defaults.

Two things worth knowing:

- **Saving preserves keys omy does not recognise.** Switching between an older and a newer version will not make them wipe each other's settings.
- **Saving loses comments.** Any comments you wrote by hand are gone after the GUI saves once.
- **Concurrent GUI + CLI writes don't clobber each other.** Adding/removing/editing locations (`remote.places`) and writing back the Telegram `api_hash` all happen inside one cross-process config lock: "read the latest on-disk copy → change only this one entry → write back atomically", instead of writing back a stale startup snapshot (which would silently delete a location the resident GUI had just added). The lock is an OS advisory lock on a sibling `config.toml.lock` (released with the handle when a process crashes, no leftover lock file); it times out after 5 s. Read paths take no lock — a temp-file-plus-rename means a reader sees either the old or the new file, never a half one.

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
# Auto-start LAN sharing on launch (GUI only)
autostart = false

[ui]
# auto | zh-CN | en
language = "en"
# auto | dark | light
theme = "dark"
# grid | list
view = "list"
# Open at startup: last (last folder) | home (GUI only)
startup = "last"
# Last browsed folder, recorded automatically when startup = "last"
# last_dir = "C:/Users/me/Vault"
# Show thumbnails in the list (they live in the file header)
thumbnails = true

[security]
# Lock after this many idle seconds; 0 means never
auto_lock_secs = 600
# Lock immediately when backgrounded (mobile app switch / desktop minimize).
# When false (default) backgrounding only starts the idle timer; playback is exempt
lock_on_background = false
# Wipe temporary plaintext when the external opener closes (GUI wiring pending)
wipe_temp_plaintext = true

[remote]
# Temporary block cache limit in bytes; 0 means unlimited
cache_limit = 2147483648
# Cache directory; empty uses the default portable path (GUI only)
# cache_dir = ""
# Clear the cache when the app exits
clear_cache_on_exit = false
# Cache on Wi-Fi only (mobile; not yet active)
cache_wifi_only = false
# Concurrent requests when scanning, 1–32; a rate-limit boundary, not a perf knob
scan_concurrency = 8
# Only look at .omy names when scanning; false detects disguised files but costs a request per file
scan_omy_only = true
```

::: tip Which keys the CLI also reads, and which are GUI-only
`remote.places`, `remote.cache_dir`, and `remote.cache_limit` are now **shared** by the GUI and the `omy remote` command: locations added on the CLI, the cache directory, and the cache limit agree on both ends.
`security.*` (auto-lock, etc.) and `remote.clear_cache_on_exit` / `scan_concurrency` / `scan_omy_only` still only affect the GUI. Both ends read the same file; keys the CLI has no use for are preserved and never reported as unknown.
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
| `serve.autostart` | `false` | Auto-start LAN sharing on launch (GUI only) |
| `serve.default_expire` | none | Default authorization lifetime |
| `ui.language` | `"auto"` | Interface language; `auto` follows the system locale |
| `ui.theme` | `"auto"` | Interface theme; `auto` follows the system (GUI only) |
| `ui.view` | `"grid"` | Default view, `grid` or `list` (GUI only) |
| `ui.startup` | `"last"` | Open at startup: `last` (last folder) or `home` (GUI only) |
| `ui.last_dir` | none | Last browsed folder, recorded automatically (GUI only) |
| `ui.thumbnails` | `true` | Show thumbnails in the list (GUI only) |
| `security.auto_lock_secs` | `0` | Lock after this many idle seconds; `0` never (GUI only) |
| `security.lock_on_background` | `false` | Lock **immediately** when backgrounded; when off, backgrounding only counts as idle and playback is exempt (GUI only) |
| `security.wipe_temp_plaintext` | `true` | Wipe temporary plaintext when the external opener closes (GUI wiring pending) |
| `password_managers.keepassxc.proxy_path` | none | Custom KeePassXC proxy path; empty enables automatic discovery. The desktop GUI exposes it under **Settings → Security → Portable KeePassXC** |
| `password_managers.keepassxc.associations` | `[]` | Public hashes and names of paired databases; authorization keys live in the OS credential store |
| `remote.cache_limit` | `2147483648` | Temporary block-cache limit, 2 GiB; `0` unlimited (shared by the GUI and `remote cache`) |
| `remote.cache_dir` | none | Custom cache directory; empty uses the default portable path (shared by the GUI and `remote cache`) |
| `remote.clear_cache_on_exit` | `false` | Clear the temporary block cache on exit (GUI only; permanent files are unaffected) |
| `remote.cache_wifi_only` | `false` | Cache on Wi-Fi only (mobile; not yet active) |
| `remote.scan_concurrency` | `8` | Concurrent requests when scanning, 1–32 (GUI only) |
| `remote.places` | `[]` | Saved remote locations, maintained jointly by the GUI and the `remote` CLI; passwords are stored there as encrypted envelopes |
| `remote.telegram_api_id` | none | Custom Telegram api_id (integer); blank uses the built-in identity (shared by GUI and the `telegram app-id-*` CLI) |
| `remote.telegram_api_hash` | none | Custom api_hash, sealed by the OS credential store into an encrypted envelope; a plaintext value written by an older version is automatically re-sealed inside the cross-process lock, and plaintext is refused when no credential store is available |
| `remote.scan_omy_only` | `true` | Whether remote scanning only looks at `.omy` (GUI only) |

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
