---
title: Remote locations (cloud / WebDAV)
---

# Remote locations (cloud / WebDAV)

Mount a NAS, cloud drive, or any WebDAV-capable service as a **remote location**, browse its `.omy` files like a local folder, and **stream videos and preview images directly — only ciphertext ever leaves the device, and decryption happens on your machine**.

The first supported protocol is **WebDAV**. It is the common standard across most self-hosted NAS (Synology, QNAP, TrueNAS) and aggregators (AList, Nextcloud, ownCloud), so a single WebDAV driver covers a large class of storage without adapting to each vendor.

::: tip Why WebDAV first
WebDAV is an open standard with uniform semantics for listing directories, random-access reads via `Range`, upload, delete, and rename. Private cloud APIs differ widely in client protocol and token policy, and will be added as separate drivers only when there is a real need.
:::

## Add a remote location

Open **Connect remote location** in the GUI and fill in:

- **Server URL**: the WebDAV root, e.g. `https://nas.example.com/dav/`;
- **Username / password**: credentials for that service;
- **Name**: the label shown in the sidebar (can be left blank);
- **Writable**: leave it read-only if you are unsure.

The app runs a read-only probe first: the location is added only if it can connect and list the directory. If the account lacks write permission, it is automatically marked **read-only**.

## Browse, recognize, and stream

Once inside, the app lists the directory and recognizes `.omy` files:

- Recognition reads the file header rather than relying on the name, so renamed or moved files are classified correctly;
- Video, audio, and images use the same preview/playback pipeline as local files. Seeking fetches ciphertext blocks on demand via HTTP `Range` — the whole file is **never** downloaded first;
- In grid view, unlocked entries whose header carries a thumbnail show it directly. The thumbnail lives in the header TLV and is obtained while listing the directory, so **no body download is involved**; on failure it falls back to a type icon;
- Listing is **progressive**: once the directory is listed, "Identifying…" skeletons appear immediately and each entry is filled in as soon as it is recognized — you don't wait for every file to finish;
- A single file whose header can't be fetched because of a network problem is marked separately as "Could not read"; **tap the entry to retry it**. It is never confused with "locked (wrong password)", and you don't have to reload the whole directory;
- Besides the item count, the bottom status bar separately counts 🔓 unlocked and 🔒 still-locked (wrong or untried password) encrypted files in the current directory. The numbers update live as scanning progresses; entries still being identified are not counted yet;
- Decryption happens locally. The server and the wire only ever see ciphertext, just like the [LAN sharing](lan-sharing) model: you must know the password yourself.

## Read-only vs writable

Capabilities come from the backend's real probe, not from a UI toggle:

| Operation | Writable | Read-only |
|---|---|---|
| List, recognize `.omy` | ✅ | ✅ |
| Preview / stream (read ciphertext on demand) | ✅ | ✅ |
| Listing thumbnails (read header, not body) | ✅ | ✅ |
| Decrypt to local (plaintext lands only on this machine) | ✅ | ✅ |
| Ciphertext block cache | ✅ | ✅ |
| Remove a single file from the cache (local ciphertext only) | ✅ | ✅ |
| Encrypted upload, new folder, rename, delete | ✅ (planned) | ❌ rejected before any request is sent |

**Right-click on desktop, long-press on mobile** on an entry opens its context menu, which currently offers "Preview / Open", "Decrypt to local…", and — once that file actually has local ciphertext blocks cached — "Remove from cache".

### Decrypt to local

A read-only location cannot be rewritten, yet you may still need to bring a file back to this machine to edit or keep — that is what "Decrypt to local" is for: the server only ever holds ciphertext, and plaintext is written only into a local folder you choose.

- After you pick a local folder, the app **streams** ciphertext, decrypts, and writes it out chunk by chunk; a multi-GB video is never loaded fully into memory.
- Downloads reuse the ciphertext block cache, so parts already streamed while scrubbing are not fetched twice.
- The file name comes from the original name recorded in the header (and is sanitized to stop a crafted name from writing outside the target folder). It **never overwrites**: if a same-named file already exists, it errors out and asks you to pick another location.
- Encrypted folders (containers) cannot yet be decrypted to local from a remote location; handle them locally for now.

Operations that rewrite the remote side (encrypted upload, delete, rename, new folder) do not have an entry point yet. When they arrive, they will be **hidden entirely** (not greyed out) on a read-only location, and only greyed out with a note under the item — left-aligned with its label — where the location is writable but cannot rewrite in place, so you never walk half-way into an action the location cannot accept.

## Ciphertext cache

Repeatedly seeking through a remote file would otherwise re-fetch the same ciphertext. The app keeps a local block cache that stores **ciphertext only**:

- Blocks are aligned to 1 MiB and evicted with LRU once usage exceeds the limit;
- **Plaintext and keys are never cached** — a copied cache directory contains nothing but ciphertext;
- The cache key incorporates the file header and total length, so updated content under the same name never matches a stale cache.

Under **Settings → Remote locations → Ciphertext cache** you can:

- Set the maximum size (512 MB / 1 GB / 2 GB / 5 GB / 10 GB / unlimited; default 2 GB);
- See current usage and clear it immediately;
- Open the cache directory;
- Clear the cache on exit.

Beyond clearing everything in settings, you can clean up a single file: choose "Remove from cache" in an entry's context menu. It deletes only that file's locally downloaded ciphertext blocks and never touches the remote file, so it is available on read-only locations too. The menu queries on demand — the item only appears when the file actually has some cached blocks; a file with no cache never shows an empty action. Whether a file is fully cached (available offline) is counted block by block at 1 MiB; this is not scanned for every file while listing the directory, to avoid slowing down browsing when there are many files.

"Cache on Wi-Fi only" on mobile will follow in a later release.

## Where config and cache live

Portable-first layout:

- **Desktop**: `omy-data/config.toml` and `omy-data/cache/remote/` sit next to the executable; copying that folder carries settings and cache with it;
- **Mobile**: the app's own data directory.

See [Configuration](configuration) for the full list of keys.

## How passwords are stored

Remote locations are saved along with your configuration and come back after a
restart. The **password is never written to the configuration file in plain
text**: it is encrypted with a key, and that key is handed to the operating
system to keep.

| Platform | Where the key lives |
|---|---|
| Windows | Credential Manager |
| macOS | Keychain |
| Linux | Secret Service (gnome-keyring / KWallet, etc.) |
| Android | The app's private directory |

This means **copying the configuration file to another machine gets you
nothing** — the key is not there. Equally, if you move to a new computer or
clear your keychain, the locations are still listed but you have to enter the
password again.

::: warning What it does and does not protect against
It protects against: the configuration file being copied, the disk being pulled
and mounted elsewhere, a leaked backup.

It does **not** protect against an attacker who can run programs as you on this
machine — they can simply ask the system for the key. That is an inherent limit
of OS credential stores, not something specific to omy. Defending against it
needs a "verify your fingerprint every time" tier, which does not exist yet.
:::

::: tip Linux needs a keyring service
On Linux something implementing Secret Service must be present (installing a
desktop environment usually brings one). Without it omy does **not** fall back
to storing the password in plain text — the location is still saved, the
password is not, and you enter it each time you start. Settings shows which
state you are in.

The kernel keyring (keyutils) would also work, but it is cleared on reboot,
which would mean re-entering the password after every restart, so omy does not
use it.
:::

The **name, address and username are stored in plain text**; only the password
is encrypted. This is deliberate: without the key those locations must still be
listed so you can be told to log in again, rather than showing an empty list
that looks like lost configuration. The cost is that anyone reading the file can
tell which server you use.

## Current limitations (first iteration)

- **Remote write operations** — encrypted upload, new folder, rename/delete, transfer progress — are not available yet; this iteration focuses on remote browsing, streaming, and decrypt-to-local. Decrypting an encrypted folder (container) from remote to local is not supported yet either.
- No private (non-WebDAV) cloud drivers yet.
- No "unlock with fingerprint / face" tier for credential protection yet.

The design and progress notes live in `docs/research/14-remote-locations-cloud.md` in the repository.
