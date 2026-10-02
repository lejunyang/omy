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
| `upload <location> <local-path> <remote-dir>` | Upload a local file; if `<local-path>` is a **directory, recurse the whole tree** (see below) |
| `download <location> <remote-file> <local-path>` | Download **ciphertext** to local in 1 MiB chunks, streamed to disk (no decryption) |
| `copy <src-loc>:<file> <dst-loc>:<path>` | Copy a remote file verbatim (ciphertext bytes) between two locations |
| `mkdir <location> <remote-dir>` | Create a remote directory, creating missing parents |
| `delete <location> <remote-path>` | Delete a remote file or directory (directories recurse; irreversible; confirm by default, `--force` skips it) |
| `move <location> <src> <dst>` | Rename/move within the same directory; cross-directory move is not supported and errors out |
| `decrypt <location> <remote.omy> <local-dir>` | Stream a single remote `.omy` down-and-decrypt into a local directory |

These write operations take `--force`: by default they **refuse to overwrite** an existing same-name target — add it to confirm. `upload` overwrites an existing remote file, `download` / `decrypt` overwrite an existing local file, `copy` overwrites an existing target remote file. For `delete`, `--force` skips the "permanent delete" confirm.

Real behavior worth knowing:

- **`upload` takes a file or a whole directory.** A local file uploads that file; a local directory recurses the whole tree, mirroring it under `<remote-dir>/<dir-name>/…`. A directory upload is **not transactional** — one file failing does not roll back its already-uploaded siblings; the command records `files_ok` / `files_failed` per item and exits non-zero if any failed, saying so explicitly.
- **Commit is an atomic temp-then-rename.** When the target supports rename/delete, the upload writes a random temp name and MOVEs it into place on success, cleaning up the temp object precisely on failure; targets without rename write the final name directly and clean up best-effort, surfacing any residual risk in the error. `upload` / `copy` / `decrypt` all share this commit logic.
- `copy` copies the **raw bytes (ciphertext)** — both ends need no shared password. The transfer is **bounded and streamed** (a 64 KiB backpressured pipe, only a block or two in memory), so large files do not get aggregated into memory.
- `move` is currently a **same-directory rename** (distinct from `remote rename <location> <new-name>`, which changes the local display name). Cross-directory move is limited by the store abstraction and is rejected before any request.
- `decrypt` handles a **single `.omy` file** only: remote directories and encrypted folders (containers) are refused. It reads only the header and then decrypts chunk-by-chunk on the fly, never landing the whole ciphertext first; the filename comes from inside the header and is sanitized, and the local output directory is created if missing. The file password comes via `--password-*` or an interactive prompt.
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

#### Login and session

| Subcommand | Purpose |
|---|---|
| `login` | QR-code log in to a Telegram account and save it as a location |
| `phone` | Phone number + code + two-step-password login |
| `logout <location>` | Remove the location **and destroy** the local session (must log in again to use it) |
| `detach <location>` | Remove from the list but **keep** the local session (can be re-added without logging in again) |
| `tdata probe` | Probe common local tdata locations (read-only) |
| `tdata check <dir>` | Check whether a directory looks like tdata (structure only, no decryption) |
| `tdata import <dir>` | Import a session from desktop tdata: ask the server first, adopt only on success |
| `status [location]` | Show local session status for Telegram locations (**offline**) |

`login` options:

| Option | Notes |
|---|---|
| `--name <name>` | Location name; defaults to the server-side nickname |
| `--proxy <URL>` | Proxy, e.g. `socks5://127.0.0.1:7897`; defaults to auto-detecting the system proxy |
| `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>` | Two-step password source, pick one |

`phone` adds `--phone <number>` (international format, e.g. `+861…`; may be omitted interactively) and a code channel `--code-env` / `--code-file` / `--code-stdin`. `tdata import` adds `--name`, `--proxy`, and the tdata local passcode `--passcode-stdin` / `--passcode-file` / `--passcode-env`.

The QR code is printed to **stderr**: half-block Unicode in a real terminal, falling back to plain ASCII when piped/redirected (non-TTY), and a copyable `tg://login?token=...` link is **always** printed so you can open it elsewhere on your phone. The code refreshes automatically when it expires. The two-step password never appears on the command line; in a non-interactive (piped) run a wrong password fails after a single attempt instead of waiting on the pipe.

::: warning The successful `--json` result carries no login ticket
That `tg://login?token=...` line is a short-lived scannable ticket; it goes to **stderr only, never into the `--json` stdout** — otherwise `... --json | tee …` would persist it into a log, effectively saving a link anyone could scan to log in. The success JSON contains only `id` / `name` / `duplicate` / `user_id` / `session_saved`.
:::

After a successful login the account is de-duplicated by `user_id`: logging in again for the same account adopts the existing location instead of creating a new one. Once saved, `ls` / `upload` / `download` / `copy` / `cache` work on the Telegram location too (each conversation is a directory).

#### Proxy and connectivity

| Subcommand | Purpose |
|---|---|
| `proxy-status` | Show the currently effective global Telegram proxy |
| `proxy-set --url <URL>` | Set a manual global proxy (`http://` or `host:port` is normalized to SOCKS5) |
| `proxy-set --system` | Follow the system proxy instead (mutually exclusive with `--url`) |
| `proxy-reset` | Go back to following the system proxy |
| `check` | Pre-login connectivity self-check: TCP to Telegram's primary DC via the configured proxy only; **exit code 1 when unreachable** |

This is a **global** policy shared by every account and every login / browse / forward — the same store as the GUI's "Settings › Telegram proxy".

#### App identity (api_id)

| Subcommand | Purpose |
|---|---|
| `app-id-status` | Show the current app identity (built-in or custom api_id) |
| `app-id-set --api-id <ID> --hash-*` | Save a custom api_id / api_hash; `api_hash` comes via an env/file/stdin channel and is **sealed into an encrypted envelope**, never argv |
| `app-id-reset` | Go back to the built-in app identity |

A custom `api_hash` is sealed by the OS credential-store master key before being written; with no usable credential store the CLI **refuses to write plaintext** and makes you use the built-in identity. A config where an older version stored the `api_hash` as plaintext is automatically re-sealed into an envelope inside the config cross-process lock on next read (with no credential store it is left as-is rather than dropped).

#### Per-place encryption (offline)

These only transform the on-disk session envelope; they **do not touch the network**:

| Subcommand | Purpose |
|---|---|
| `encrypt <location>` | Independently encrypt this location's session with an interactive password (KDF default `moderate`, `--kdf` to override) |
| `unlock <location>` | Verify the interactive password can unlock it (one-shot process; verification only, not persisted) |
| `lock <location>` | Confirm the location is encrypted and locked (CLI has no long-lived session; touches no disk) |
| `decrypt <location>` | Remove per-place encryption, back to machine-key protection |

The place password always goes via `--password-stdin` / `--password-file` / `--password-env`. The CLI has no long-lived session like the GUI: `unlock` only answers "is the password right?" — the process exits and the key in memory is gone, so the next access must supply the password again via a file command's `--password-*`. `lock` only confirms it is an encrypted location; an unencrypted one errors with `[tg_not_encrypted]` instead of pretending to be locked. Errors are stable bracket-prefixed strings (`[tg_unlock_wrong]` wrong password / `[tg_no_protector]` no credential store, refusing plaintext / `[tg_no_session]` no session yet), all with exit code 1 — scripts should match the prefix, not the exit code.

#### Online operations (require a real Telegram connection)

| Subcommand | Purpose |
|---|---|
| `targets <location>` | List the conversations you can forward to |
| `forward <location> --target tg:CHAT --entry tg:CHAT:MSG…` | natively forward one or more messages from one source chat to a target |
| `search <location> <QUERY>` | Server-side message search (`--dir tg:CHAT` to scope to one chat, global by default; `--limit N` default 50) |
| `group <location> <TITLE>` | Create a private supergroup containing only yourself (a forwarding archive target) |

These require a real Telegram connection; an encrypted location must be unlocked on the spot via `--password-*`. Forward sources can be given repeatably as `--entry tg:<chat>:<msg>`, or read as an id array from `--entries-json <file>` (may be combined); **all entries must come from the same source chat**, else `[tg_cross_chat]`.

::: warning The real-account login / forward / search leg is not end-to-end tested headless
The wiring of `phone` / `tdata import` / `targets` / `forward` / `search` / `group` and their failure reporting is done, and the offline parts (place-envelope encryption, proxy config, connectivity self-check, virtual favorites) are pinned by unit tests plus WebDAV end-to-end against a local dav-server. But actually reaching Telegram's data centers and completing a real-account scan / code / forward / search cannot be done headless, and is **not claimed as verified**. See the coverage table at the end of [Remote locations](../guide/remote-locations).
:::

### Virtual remote locations (favorites)

```
omy remote virtual <COMMAND>
```

A virtual location does not connect to any server; it only stores local **references** to real remote files (a favorites folder). This whole tree makes **no network requests** and shares the same model and on-disk format as the GUI.

| Subcommand | Purpose |
|---|---|
| `place create <name>` / `list` / `show <location>` / `rename <location> <name>` / `remove <location>` | Manage virtual locations (`remove` deletes the location and its favorites, never the real remote files) |
| `folder add <location> <name>` / `rename <location> <folder-id> <name>` / `remove <location> <folder-id>` | Virtual folders (`remove` takes the whole subtree of references); use `--root` for the root or `--folder <id>` for a parent |
| `ref add <location>` | Favorite a real remote file: `--source-place <p…>` / `--dir-id` / `--file-id` / `--name` (`--folder` picks the destination folder) |
| `ref remove <location> <ref>` / `move <location> <ref>` / `copy <from-location> --ref-id <ref> --to-place <location>` | Organize references (`remove` never touches the real file; `move` stays in one place, `copy` may cross places and mints new ref ids) |
| `ref list <location>` | Recursively list every reference under a place / folder |
| `ref browse <location>` | Single-level browse: immediate subfolders and references |
| `encrypt <location>` / `unlock <location>` / `lock <location>` / `decrypt <location>` | Independently encrypt / verify password / confirm locked / remove encryption |

Virtual location ids look like `v1`, `v2`. **One-shot process semantics:** every CLI command reloads config and favorites fresh, so an encrypted virtual location starts locked. Read/modify commands therefore all take the same `--password-*`; without a password you get `VIRTUAL_LOCKED`.

- `unlock` only **verifies the password**: success is not persisted or written to disk; the next access still needs the password once the process exits.
- `lock` is an honest confirmation: between one-shot CLI processes an encrypted location is locked by nature, so it touches no disk; an unencrypted location errors with `VIRTUAL_NOT_ENCRYPTED` instead of falsely reporting "locked".
- `encrypt` encrypts the whole favorites tree with an independent password; `decrypt` unlocks it and writes back plaintext favorites.
- Exit codes: 0 success, 2 usage error, **3 on wrong virtual password** (`VIRTUAL_WRONG_PASSWORD`), 8 on cancel, 1 for other virtual failures. The machine-readable code is in `--json` as `error.code` (a `VIRTUAL_*` prefix) — match that.
- References store a **stable identifier** of the source (Telegram `user_id`, or WebDAV url+account); if the source location is removed and re-added, references re-bind. No operation touches the real remote files.

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
