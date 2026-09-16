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
- Decryption happens locally. The server and the wire only ever see ciphertext, just like the [LAN sharing](lan-sharing) model: you must know the password yourself.

## Read-only vs writable

Capabilities come from the backend's real probe, not from a UI toggle:

| Operation | Writable | Read-only |
|---|---|---|
| List, recognize `.omy` | ✅ | ✅ |
| Preview / stream (read ciphertext on demand) | ✅ | ✅ |
| Listing thumbnails (read header, not body) | ✅ | ✅ |
| Ciphertext block cache | ✅ | ✅ |
| Encrypted upload, new folder, rename, delete | ✅ (planned) | ❌ rejected before any request is sent |

Operations that rewrite the remote side (encrypted upload, delete, rename, new folder) do not have an entry point yet on read-only locations. When write operations arrive, they will be **hidden entirely** (not greyed out) on a read-only location, and only greyed out with a "decrypt to local first" hint where the location is writable but cannot rewrite in place, so you never walk half-way into an action the location cannot accept.

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

"Cache on Wi-Fi only" on mobile will follow in a later release.

## Where config and cache live

Portable-first layout:

- **Desktop**: `omy-data/config.toml` and `omy-data/cache/remote/` sit next to the executable; copying that folder carries settings and cache with it;
- **Mobile**: the app's own data directory.

See [Configuration](configuration) for the full list of keys.

## Current limitations (first iteration)

- **Remote locations are kept in memory for the current session only**; after a restart you re-add and re-authenticate. Persisting locations and encrypting credentials is the next problem to solve.
- **Write operations** — encrypted upload, decrypt-to-local, new folder, rename/delete, transfer progress — are not available yet; this iteration focuses on remote browsing and streaming.
- No private (non-WebDAV) cloud drivers yet.

The design and progress notes live in `docs/research/14-remote-locations-cloud.md` in the repository.
