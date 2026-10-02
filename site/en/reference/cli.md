---
title: Command overview
---

# Command overview

```
omy <command> [options]
```

This page reflects the actual output of `omy --help`. Use `omy <command> --help` at any time for the authoritative version.

## All commands

| Command | Purpose |
|---|---|
| `encrypt` | Encrypt files or directories |
| `decrypt` | Decrypt files |
| `info` | Show file information (partly readable without a password) |
| `verify` | Verify integrity |
| `list` | List `.omy` files in a directory |
| `scan` | Scan a directory, match passwords and list unlockable files |
| `cat` | Decrypt to standard output (pipe friendly) |
| `key` | Key slot management |
| `shard` | Split and merge shards |
| `share` | LAN sharing and access |
| `remote` | Remote locations (WebDAV / Telegram): register, browse, upload/download, cache |
| `bench` | KDF and cipher throughput benchmarks |
| `doctor` | Environment self-check |
| `completion` | Generate shell completion scripts |

## Global options

Valid for every subcommand:

| Option | Purpose |
|---|---|
| `-v, --verbose` | Verbose output, repeatable (`-vv`) |
| `-q, --quiet` | Errors only. Conflicts with `-v` |
| `--json` | JSON output for scripts |
| `--no-color` | Disable coloured output |
| `--config <PATH>` | Configuration file path |
| `--yes` | Skip all confirmations (dangerous) |
| `-h, --help` | Help |
| `-V, --version` | Version |

## Password input options

Supported by `encrypt` / `decrypt` / `verify` / `cat` / `info` / `scan` / `key`:

| Option | Notes |
|---|---|
| (none) | Interactive, not echoed. Default and preferred |
| `--password-stdin` | Read from standard input |
| `--password-file <PATH>` | Read the first line of a file |
| `--password-env <VAR>` | Read from an environment variable; pass its **name** |

**There is no `--password <plaintext>`.** Using it yields an explanation and exit code 2. For `scan`, `--password-file` and `--password-env` repeat to supply several passwords.

## encrypt

```
omy encrypt [OPTIONS] <PATHS>...
```

| Option | Notes |
|---|---|
| `-o, --output <PATH>` | Output path (single input only) |
| `--output-dir <DIR>` | Batch output directory |
| `--name-mode <MODE>` | `encrypt` / `keep-ext` / `plain` |
| `--add-password` | Add a key slot, repeatable, up to 8. Interactive only |
| `--kdf-profile <PROFILE>` | `mobile` / `interactive` / `moderate` / `sensitive` |
| `--vault <PATH>` | Join an existing vault, reusing its salt and KDF parameters |
| `--cipher <CIPHER>` | `xchacha20` / `aes256gcm` |
| `--chunk-size <SIZE>` | Such as `256K` / `1M` / `4M` |
| `--compress` | Enable zstd compression |
| `--compress-level <N>` | zstd level 1-19 |
| `--mode <MODE>` | `container` (default) / `tree` |
| `--slot-mode <MODE>` | Password management: `deniable` (default, nobody can tell how many passwords exist) / `managed` (slots can be listed and removed precisely; fixed at encryption time) |
| `--original <ACTION>` | `keep` (default) / `trash` / `delete` |
| `--thumbnail <MODE>` | `auto` (default) / `none` |
| `--thumbnail-frame <T>` | Video frame timestamp, e.g. `00:01:23` / `83` / `83.5` |
| `--no-media-meta` | Do not write media metadata |
| `--no-moov-cache` | Do not cache the MP4 moov box |

The output name **appends** `.omy` to the full filename: `note.txt` → `note.txt.omy`.

## decrypt

```
omy decrypt [OPTIONS] <FILES>...
```

| Option | Notes |
|---|---|
| `-o, --output <PATH>` | Output path (single input only) |
| `--output-dir <DIR>` | Batch output directory |
| `--stdout` | Write to standard output. Conflicts with `-o` / `--output-dir` |
| `--verify-only` | Check without writing |
| `--ignore-missing-shards` | Emit the available part when shards are missing |

## info

```
omy info [OPTIONS] <FILE>
```

| Option | Notes |
|---|---|
| `--with-password` | Supply a password to see encrypted metadata (original filename etc.) |
| `--extract-thumbnail <PATH>` | Export the thumbnail (password required, WebP format) |

Without a password the public fields are visible: format version, UUID, cipher, KDF parameters, chunk size, chunk count, ciphertext and plaintext sizes. Slot occupancy always reads "unknown" — unprobeable by design.

## verify

```
omy verify [OPTIONS] <FILES>...
```

Checks every chunk's authentication tag and compares the content hash.

## list

```
omy list [OPTIONS] [DIRS]...
```

| Option | Notes |
|---|---|
| `-r, --recursive` | Recurse into subdirectories |
| `--max-depth <N>` | Maximum recursion depth |
| `--any-extension` | Also list files whose header matches but extension is not `.omy` |

Needs no password — it lists files without attempting decryption. Defaults to the current directory.

## scan

```
omy scan [OPTIONS] [DIRS]...
```

| Option | Notes |
|---|---|
| `-r, --recursive` | Recurse (**on by default**) |
| `--no-recursive` | Only the top level |
| `--max-depth <N>` | Maximum recursion depth |
| `--show-locked` | Also list files that did not match |
| `--any-extension` | Also check files not ending in `.omy` (recognition uses the header magic, so renamed files are found too) |
| `--max-files <N>` | Cap on files examined per scan |

Listed names are the **decrypted originals**.

## cat

```
omy cat [OPTIONS] <FILE>
```

| Option | Notes |
|---|---|
| `--range <RANGE>` | Emit only this range, `start-end`, endpoints inclusive |

Supports the HTTP Range suffix form: `--range -256` takes the last 256 bytes.

## key

```
omy key <COMMAND> <FILE>
```

| Subcommand | Purpose |
|---|---|
| `add` | Add a key slot (requires one existing password) |
| `remove` | Keep only the current password, voiding all others **including the recovery code** |
| `list` | Show slot occupancy |
| `change` | Change the password (the old one stops working; other slots are kept) |
| `reencrypt` | Re-encrypt: replace the file key and rewrite the payload (may change the password too) |
| `recovery` | Generate a recovery code and attach it to the file |
| `restore` | Open the file with a recovery code and set a new password |

`add` and `change` take the new password via `--new-password-file` / `--new-password-env`.

In deniable mode (the default) `key list` needs no password: slot occupancy is
unprobeable anyway, so a password adds nothing.

For a file in manageable mode you can supply one, and it lists what each slot holds:

```bash
omy key list file.omy --password-env PW
omy key list <encrypted folder> --password-env PW   # folders work too
```

| Option | Description |
|---|---|
| `--password-env <VAR>` | Read the password from an environment variable |
| `--password-file <PATH>` | Read the password from a file |
| `--password-stdin` | Read the password from stdin |

Requiring a password is by design — the slot directory is encrypted, so reading it
means opening the file first.

`encrypt` takes `--slot-mode <deniable|managed>` to pick the mode; the default is
`deniable`. The mode lives in the file header and cannot be changed after encryption.

### How change differs from remove

`change` and `add` carry the file's other slots over **untouched**, so you can keep
changing passwords after setting a recovery code. Only `remove` wipes: it voids every
other password, recovery code included.

`add` claims a new slot, and the implementation cannot probe which slots are free
(exactly what deniability requires), so it may overwrite whatever sat at that index.
The command warns honestly, but it cannot tell you whether a real password was there.

### recovery / restore

```bash
omy key recovery secret.omy                  # prints 26 words
omy key recovery secret.omy --out code.txt   # write to a file instead of the terminal
omy key restore secret.omy --code-file code.txt
```

Whatever `--out` writes is plaintext; move it somewhere safe immediately. The recovery
code never appears in `--json` output — that output is routinely redirected into files
or piped into logs.

`restore` invalidates **every other everyday password** on the file, keeping only the
new one and the recovery code itself. This is deliberate: reaching for the recovery
code usually means the old password was forgotten or may have leaked, so keeping the
others serves no purpose. The recovery code is always kept — you have just been locked
out once, and removing your only fallback at that moment is the worst possible timing;
the same code can be used again later.

In manageable mode it also reports how many were invalidated.

See [recovery codes](../guide/passwords.md#recovery-codes-the-only-way-back-in).

### device (device key)

Unlock with Windows Hello instead of typing the password every time.

```bash
omy key device secret.omy add        # enroll (asks for the password once)
omy key device secret.omy status     # check whether this machine has one
omy key device secret.omy remove     # remove it
omy decrypt secret.omy --device      # unlock with it
```

It is an ordinary password slot; the only difference is where the key comes from.
A password slot's key is derived from the password; a device key is **randomly
generated** and its ciphertext is kept by this machine's TPM. Retrieving it requires
Windows Hello confirmation.

::: warning It does not make the file safer, only more convenient
The TPM protects against a stolen disk and against decryption on another machine. It
does **not** protect against malware running as you on this machine — that can request
a decryption in the window right after you touch the sensor.

So do not weaken your password because this is enabled. A device key saves typing,
not protection.
:::

::: danger It can be lost permanently at any time
Replacing the machine, reinstalling the OS, clearing the TPM, resetting Windows
Hello — any of these makes the device key **unrecoverable**.

It is not a backup. Keep remembering your password.
:::

One device key covers a whole vault (every file sharing the same `vault_salt`), and
different vaults are isolated automatically. Folders work too:
`omy key device <encrypted folder> add`.

Windows only for now, and it needs TPM 2.0 plus a configured Windows Hello. Other
platforms report that it is unsupported rather than silently falling back to something
without hardware backing.

## shard

```
omy shard <COMMAND>
```

| Subcommand | Purpose |
|---|---|
| `split` | Split an encrypted file into shards |
| `merge` | Merge shards |
| `check` | Check shard integrity and gaps |

Options for `split`:

| Option | Notes |
|---|---|
| `--size <SIZE>` | **Required**, size per shard, e.g. `4095M` |
| `--output-dir <DIR>` | Output directory, defaults to alongside the source |
| `--redundant-header` | Include a redundant header per shard (**on by default**) |
| `--no-redundant-header` | Omit redundant headers |
| `--remove-source` | Delete the source after splitting |

## share

```
omy share <COMMAND>
```

| Subcommand | Purpose |
|---|---|
| `serve <DIR>` | Share a directory, awaiting paired devices |
| `discover` | Search the LAN for devices currently sharing |
| `pair [ADDR]` | Pair with another device (both sides run it) |
| `connect <FINGERPRINT>` | Connect to a paired device, list or fetch files |
| `devices list/revoke/purge` | Manage paired devices |

For these subcommands `--password-*` refers to the **device store password**, unrelated to file passwords. The pairing code uses the separate `--pin-file`.

Common `serve` options: `--name`, `--port` (0 = system-assigned), `--local-only`, `--no-advertise`, `--max-connections`, `--store`.

Common `connect` options: `--addr` (skip mDNS), `--fetch <DIR>` (fetch ciphertext without decrypting).

Common `pair` options: `--listen`, `--port`, `--expires-in <DAYS>` (0 = never), `--pin-file`.

## remote

```
omy remote <COMMAND>
```

Remote locations share the **same configuration** (`remote.places`) as the GUI: a location added on the CLI is recognized by the GUI and vice versa. Location ids look like `p1`, `p2`…; `<location>` below accepts either an id or a display name — a name that matches more than one location errors out and asks you to use the id.

Exit codes: 2 for usage errors, and 1 for every other remote failure (so scripts can tell "bad invocation" from "ran but failed").

### Location management

| Subcommand | Purpose |
|---|---|
| `list` | List registered locations (id, r/w, kind, name; never the password) |
| `show <location>` | Show one location's connection info (no password) |
| `add-webdav <name> --url <URL>` | Add a WebDAV location |
| `remove <location>` | Remove a location (registration only; **cloud files untouched**, with a confirm) |
| `rename <location> <new-name>` | Change the local display name only; the server is not touched |

`add-webdav` options:

| Option | Notes |
|---|---|
| `--url <URL>` | **Required**, WebDAV root, e.g. `https://dav.example.com/dav` |
| `--user <USER>` | Username |
| `--vendor <NAME>` | `generic` (default) or `nextcloud` |
| `--read-only` | Read-only (writable by default) |
| `--anonymous` | Anonymous access: stores no username/password; mutually exclusive with any `--password-*` |
| `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>` | Password source, pick one |

Unless you pass `--anonymous`, the password is **required** (interactive hidden input / stdin / file / env); leaving it blank to mean anonymous no longer works. Before saving, it builds the client and lists the root as a liveness probe: an invalid URL or an unreachable server means the location is **not saved**. Like every other command there is **no `--password <plaintext>`**.

### Browsing and file operations

| Subcommand | Purpose |
|---|---|
| `ls <location> [path]` | List a remote directory; defaults to root |
| `upload <location> <local-file> <remote-dir>` | Upload a single local file to a remote directory (streamed) |
| `download <location> <remote-file> <local-path>` | Download to local in 1 MiB chunks, streamed to disk |
| `copy <src-loc>:<file> <dst-loc>:<path>` | Copy a remote file verbatim between two locations |

All three write operations take `--force`: by default they **refuse to overwrite** an existing same-name target — add it to confirm. `upload` overwrites an existing remote file, `download` overwrites an existing local file, `copy` overwrites an existing target remote file.

Real behavior worth knowing:

- `upload` sends **one single file** at a time; it does not upload a whole directory.
- `copy` copies the **raw bytes (ciphertext)** — both ends need no shared password. The transfer is **bounded and streamed** (backpressured chunks, only a block or two in memory at a time), so large files do not get aggregated into memory.
- A remote path containing `..` segments is rejected before any request is sent, to prevent climbing out of the intended directory; relative paths are normalized to a leading `/`.
- A read-only destination is rejected before any request is sent.

### Ciphertext cache

| Subcommand | Purpose |
|---|---|
| `cache status` | Show temp/pinned cache usage and the pinned list |
| `cache clear` | Clear temporary cache blocks (pinned files are untouched) |
| `cache pin <location> <remote-file>` | Pull a whole file locally and keep it pinned (available offline) |
| `cache unpin <location> <remote-file>` | Unpin; blocks return to the temp tier and may be evicted |

The cache directory and limit use the same keys as the GUI (`remote.cache_dir` / `remote.cache_limit`).

### Telegram

```
omy remote telegram <COMMAND>
```

| Subcommand | Purpose |
|---|---|
| `login` | QR-code log in to a Telegram account and save it as a location |
| `logout <location>` | Remove the location **and destroy** the local session (must re-scan to use again) |
| `detach <location>` | Remove from the list but **keep** the local session (can be re-added without re-scanning) |

`login` options:

| Option | Notes |
|---|---|
| `--name <name>` | Location name; defaults to the server-side nickname |
| `--proxy <URL>` | Proxy, e.g. `socks5://127.0.0.1:7897`; defaults to auto-detecting the system proxy |
| `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>` | Two-step password source, pick one |

The QR code is printed to **stderr**: half-block Unicode in a real terminal, falling back to plain ASCII when piped/redirected (non-TTY), and a copyable `tg://login?token=...` link is **always** printed so you can open it elsewhere on your phone. The code refreshes automatically when it expires. The two-step password never appears on the command line; in a non-interactive (piped) run a wrong password fails after a single attempt instead of waiting on the pipe.

::: warning The successful `--json` result carries no login ticket
That `tg://login?token=...` line is a short-lived scannable ticket; it goes to **stderr only, never into the `--json` stdout** — otherwise `... --json | tee …` would persist it into a log, effectively saving a link anyone could scan to log in. The success JSON contains only `id` / `name` / `duplicate` / `user_id` / `session_saved`.
:::

After a successful login the account is de-duplicated by `user_id`: logging in again for the same account adopts the existing location instead of creating a new one. Once saved, `ls` / `upload` / `download` / `copy` / `cache` work on the Telegram location too (each conversation is a directory).

::: warning Telegram capabilities the CLI does not cover yet
The CLI covers QR login plus basic file operations. The GUI's phone-number login, importing desktop `tdata`, message forwarding, server-side search, broadcast-channel message view, and virtual favorite locations have **no CLI equivalent**; deleting, renaming, or creating remote folders is not offered on either side. See the coverage table at the end of [Remote locations](../guide/remote-locations).
:::

## bench

```
omy bench [OPTIONS]
```

| Option | Notes |
|---|---|
| `--profile <PROFILE>` | Test only these profiles, repeatable; all by default |
| `--quick` | Skip the memory-hungry profiles (`moderate` / `sensitive`) |
| `--throughput-size <SIZE>` | Throughput test size, default `64M` |

## doctor

```
omy doctor [--probe-kdf]
```

Reports the platform, AES hardware acceleration, CPU parallelism, configuration file location, interface language, measured KDF timings, FFmpeg availability, LAN sharing availability, and known implementation limits.

Run it once after installing; it is more reliable than documentation.

## completion

```
omy completion <SHELL>
```

Supports `bash` / `elvish` / `fish` / `powershell` / `zsh`.

```bash
omy completion bash > /etc/bash_completion.d/omy
omy completion powershell | Out-String | Invoke-Expression
```
