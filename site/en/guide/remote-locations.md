---
title: Remote locations (cloud / WebDAV / Telegram)
---

# Remote locations (cloud / WebDAV / Telegram)

Mount a NAS, cloud drive, or any WebDAV-capable service as a **remote location**, browse its `.omy` files like a local folder, and **stream videos and preview images directly — only ciphertext ever leaves the device, and decryption happens on your machine**.

Two kinds of remote location are supported today: **WebDAV** and **Telegram**.

WebDAV is the common standard across most self-hosted NAS (Synology, QNAP, TrueNAS) and aggregators (AList, Nextcloud, ownCloud), so a single WebDAV driver covers a large class of storage without adapting to each vendor.

Telegram turns your private chats, groups, and channels into directories — see [Telegram](#telegram) below.

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

## Android: opening WebDAV files in other apps

On Android, the preview panel for a WebDAV file offers two explicit actions:

- **Open read-only**: grants only the selected app read access to a `content://` URI. If the same stable URI was previously opened for editing, omy revokes the old write grant first.
- **Open for editing**: shown only for writable WebDAV locations, and grants only the selected app read/write access to the same URI. Whether a third-party app actually supports in-place saving still depends on that app.

Each remote file always maps to the same private local path, so its URI remains stable across repeated opens and app restarts. The URI exposes only the working file used for external opening; it cannot be used to browse omy's state file, other caches, or general device storage.

For ordinary files, the grant is bound directly to the selected app package and does not depend on omy's UI or process remaining alive: the external app can reopen the same URI after its receiving activity has finished, and Android can start the FileProvider on demand if omy was reclaimed. Whenever another app is selected, omy first revokes the URI's previous grants, leaving only the latest app and the read-only or read/write access chosen this time. This is not a permanent grant across device reboots—after restarting the device, open the file from omy again. Force-stopping, clearing the data of, or uninstalling omy is also outside this guarantee.

Encrypted files are different: the external app receives a decrypted plaintext working copy, so it gets only a temporary grant tied to the receiving activity's lifetime. Locking omy immediately revokes read and write grants for that URI. Android cannot let a provider forcibly invalidate a file descriptor that another app already opened, so close the file in the third-party app before locking.

::: warning External opening keeps a plaintext working copy
Inline preview uses `omystream://` and does not write plaintext to disk, but third-party apps cannot consume that internal protocol. External opening therefore stores a **plaintext working copy** of the single file in omy's private Android directory. Only the selected app can access it through the temporary grant, but an attacker running as the same local user, a rooted device, or anything able to read private app data may still obtain the copy. Uninstalling omy or clearing its app data removes it.
:::

An editable file is not uploaded on every `FileObserver` event. The observer only marks it as potentially changed; omy checks immediately when it returns to the foreground and batches synchronization every 30 seconds while the process remains alive. If Android kills omy, the edit session, remote revision baseline, and local content fingerprint remain persisted; the next launch checks them again and resumes pending work.

Synchronization never overwrites blindly. omy records the server ETag when the file is opened, falling back to Last-Modified when no ETag is available; if neither exists, "Open for editing" is refused. Uploads are conditional. If another device changed the remote file, omy keeps the local edit and reports a conflict instead of silently replacing the remote version.

For a `.omy` file, the third-party app receives a decrypted single-file working copy. Synchronization re-encrypts it while preserving the original file UUID, all password slots, device-key slots, recovery-code slots, and format parameters. Encrypted directory containers cannot be edited externally. To cover the window where an upload succeeds but Android kills the process before state is committed, the pending ciphertext is persisted as a transaction file and the exact same bytes are reused after restart.

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
| Encrypted upload / Upload to another remote | ✅ | ❌ rejected before any request is sent |
| Keep permanently / stop keeping | ✅ | ✅ |
| New folder, rename, delete | Not available yet | ❌ |

**Right-click on desktop, long-press on mobile** on an entry opens its context menu: "Preview / Open", "Upload to…", "Decrypt to local…", "Keep permanently" (shown as "Stop keeping" for files already kept), and — once that file actually has local ciphertext blocks cached — "Remove from cache".

Menu items follow the capabilities of the **current directory**, not of the location. Within one Telegram location the conversation list is not writable, a read-only channel is not writable, and your own group is — so the upload button only appears where writing actually works.

### Decrypt to local

A read-only location cannot be rewritten, yet you may still need to bring a file back to this machine to edit or keep — that is what "Decrypt to local" is for: the server only ever holds ciphertext, and plaintext is written only into a local folder you choose.

- After you pick a local folder, the app **streams** ciphertext, decrypts, and writes it out chunk by chunk; a multi-GB video is never loaded fully into memory.
- Downloads reuse the ciphertext block cache, so parts already streamed while scrubbing are not fetched twice.
- The file name comes from the original name recorded in the header (and is sanitized to stop a crafted name from writing outside the target folder). It **never overwrites**: if a same-named file already exists, it errors out and asks you to pick another location.
- Encrypted folders (containers) cannot yet be decrypted to local from a remote location; handle them locally for now.

Encrypted upload is available (see the Telegram section below). Delete, rename, and new folder still have no general-purpose UI entry point; **Upload to…** uses the target's capabilities internally for safe commit and cleanup, as described next.

### Upload to another remote location

Right-click a local file or a file in a real remote location and choose **Upload to…**. The destination picker can browse WebDAV folders or Telegram conversations. It re-queries effective capabilities after every directory change, and confirmation is enabled only when that specific directory reports `write`; a location-level upper bound is never treated as proof that the current directory is writable.

- **Local → remote** uses the ordinary local-file upload path.
- **Remote → remote (Cache locally and upload)** stores downloaded ciphertext blocks in the permanent cache. A retry reuses those blocks, at the cost of local disk space.
- **Remote → remote (Upload through memory)** reads up to 8 MiB from the source at a time and connects download to upload with a fixed, bounded 64 MiB in-memory queue. It writes no local cache; if upload is slower, download waits instead of accumulating without bound.

When media is copied from Telegram to another Telegram conversation, omy reads the source message's real media attributes and preserves its Telegram classification as a photo, video, audio item, or GIF during re-upload. A video therefore stays in the Media tab instead of becoming a generic file attachment. Ordinary documents and `.omy` files remain file attachments, and non-Telegram targets such as WebDAV still receive only the original byte stream.

Commit and cleanup follow the destination's capabilities rather than assuming every remote can rename or delete:

1. If write, rename, and delete are all supported, omy writes a random temporary name and renames it only after the upload succeeds; failures can delete that exact temporary object.
2. If either rename or delete is unavailable, omy writes the final name directly. When delete is available it attempts cleanup after failure. When delete is unavailable or cleanup fails, Transfer Management reports that an incomplete object may remain and does not offer Retry until you inspect the destination.
3. Path-based destinations such as WebDAV refuse to overwrite a same-named existing file.

Every remote-copy operation appears immediately in **Transfer Management**, with progress, cancellation, and Retry for failures that are safe to retry.

#### Can Telegram resume transfers?

Download and upload have different answers:

- **Downloads can continue from an offset.** Telegram's download API accepts `offset`; in permanent-cache mode a retry reuses blocks already downloaded rather than fetching them again.
- **The upload protocol is chunked, but current omy cannot continue an entire failed upload from chunks already accepted by Telegram.** The grammers upload API used by omy creates a new `file_id` for each call and does not expose the completed-part set, so retrying an upload to Telegram starts that upload again. Temporary uploaded parts are short-lived as well.
- Memory mode keeps no source blocks, so a failed task reads the source again. Permanent-cache mode can avoid repeated downloading, but the destination upload still restarts.

## Telegram

Mount your Telegram conversations as a remote location: **each conversation is a directory**, and the documents, photos, and videos in it are the entries. `.omy` files are recognized, previewed, and streamed exactly as they are over WebDAV.

### Signing in

Click the Telegram icon in the sidebar. There are three routes:

- **Sign in with a QR code**: scan it with a phone that is already signed in, so you never type a phone number or verification code into omy. The code refreshes itself when it expires, and the interface makes that refresh visible. If your account has two-step verification enabled, omy asks for the cloud password at the point where the server requires it — accounts without it never see that step.
- **Sign in with a phone number**: enter a number with its country code, submit Telegram's verification code, and enter the cloud password if two-step verification is enabled.
- **Reuse the desktop sign-in**: read the `tdata` folder of Telegram Desktop on this machine and reuse the sign-in it already has, with no QR code. You must **quit** the desktop client first — closing its window is not enough, it minimises to the tray. The portable build keeps `tdata` next to `Telegram.exe`, where automatic detection usually will not find it, so the interface lets you point to it.

::: warning Reusing the desktop sign-in means sharing one session
After importing, omy and the desktop client **share a single sign-in session**, not two: signing out in the desktop client ends it in omy as well.
:::

The sign-in (which contains account credentials) is encrypted and stored on this machine. The key is held by the system credential store, and **omy never falls back to plain text** when that store is unavailable, at the cost of signing in again on every launch.

**Encryption is off by default and optional.** Accounts without an omy password stay as they are and connect automatically as before. To add a layer of omy-password protection to a sensitive account, there are two entry points:

- **Right-click it in the sidebar → "Encrypt this location"**: a password dialog opens and you **set a password right there** (typed twice to confirm, with an optional KDF strength) — the same interaction as encrypting an ordinary omy file; the same menu also offers "Remove encryption" to revert to the default.
- **After an account is added successfully**, omy asks once whether to encrypt — you can encrypt or skip (default is not encrypted).

The key is the password you set. It shares the **same session key pool** as ordinary .omy files and other encrypted locations: omy remembers every encrypted vault it has seen (only the public salt and Argon2 parameters from the file header, never the password), so with the same password, unlocking a local file first or any one location first automatically unlocks all other same-password vaults and Telegram locations without retyping; the "N passwords unlocked" indicator counts distinct passwords.

**Once encrypted**, the location shows as encrypted (with a lock badge). On a cold start, before you type the password, it shows as **locked** — you can see its name but can't enter, with a prompt to unlock first; after you enter the correct password you're in and it connects automatically. The name stays visible while locked. **Locations that are not encrypted are unaffected** and open directly.

::: tip Most networks need a proxy
Direct connections to Telegram's data centres fail on many networks. Telegram uses one **global proxy policy**, managed under **Settings → Remote locations → Telegram proxy**. Every account and every path—QR sign-in, phone sign-in, tdata import, browsing, downloads, uploads, remote copies, message reading, forwarding, and group creation—uses that same policy.

The default **Use the current system proxy automatically** mode reads the system setting whenever a Telegram connection is created or rebuilt; if no supported system proxy is enabled, it connects directly. You can instead choose **Set manually** and enter one global fixed address, such as `socks5://127.0.0.1:7897`; an `http://` address is normalized to SOCKS5 on the same port. On desktop the controls are stacked at full width in Remote locations; on mobile, Settings has a dedicated **Telegram proxy** entry that opens the same global settings. **Save and check connection** first persists the current policy, then runs the connectivity probe through the backend policy that will actually be used. Saving a different policy invalidates existing Telegram connections and open remote-file handles, and the next operation rebuilds them under the new policy.

Telegram location records **never store proxy addresses**. On upgrade, proxy values written into each location's `url` field by older versions are removed automatically and no longer participate in connection selection.
:::

omy reports itself **honestly as omy** in Telegram's list of active sessions rather than impersonating an official client, so you can always recognise it there and revoke it.

### Multiple accounts

You can mount several Telegram accounts at once. Each one is its own remote location with its own stored sign-in. Adding another account after the first still runs a full sign-in rather than silently reusing the account you already have.

A location is named after that account's display name by default. When two accounts share a display name (a main and an alt belonging to the same person, say) the sidebar cannot tell them apart, so the name **can be changed**: click the ✎ on that row in the location list. This only changes the name on this machine; it does not touch the account profile on the server.

There are two ways to get rid of a Telegram location, and their consequences differ a lot, so they are two separate entries:

| | Sign-in | Next time |
|---|---|---|
| **Remove from list** (✕) | Kept on this machine | Add it back directly, no new QR code |
| **Delete account** (🗑️) | Deleted as well | Scan a QR code or import tdata again |

The latter cannot be undone, and says so before you confirm. Note also that **removing a location from the list does not delete files you kept permanently** — they still occupy disk space; clear them from the permanent list in Settings.

### Browsing and searching

Entering the location shows the conversation list; opening a conversation shows its files. The file list is split into five tabs by type — **Media, Files, Links, Audio, GIF** — each returned by Telegram's server-side type index: the Media tab holds images and videos, the Files tab holds documents, and the rest each get their own tab. Images sent as photos and videos sent as videos land in the Media tab; documents sent as files land in the Files tab. Tabs load a screen at a time — fill one screen first, then fetch more as you scroll. Thumbnails first show the tiny low-res placeholder carried with the message, then swap to the sharper image once it is fetched in the background.

Each conversation also has a **message view** that lists messages over time with files as the through-line, so a message carrying a file can be opened directly. For broadcast channels the message view is **read-only browsing** (you can read it, but omy does not implement sponsored messages — see the note below).

When a message carries a "reply #N" reference, clicking it jumps to the referenced message: while loading, only a small spinner appears next to the reference — the whole list is not reloaded. If the target is outside the loaded range, omy fetches a window centered on it and highlights the message. After the jump the timeline **continues in both directions** — scroll down for older messages and up (toward the top) for newer ones; the messages from before the jump are kept.

For the file block inside a message: a plain click **previews/opens** it; right-click and choose "Show in files" to switch to the matching file tab (Media / Files / Links / Audio / GIF, chosen from the file's type) and scroll that file card into the middle with a highlight. If the target is outside the loaded range, omy fetches one page anchored on it — it never triggers endless paging.

### Multi-select and forwarding

Files and messages inside a Telegram conversation can be forwarded natively:

- In file view, right-click one file and choose **Forward…**. Use Ctrl/Command-click on desktop or long-press on mobile to enter multi-select; the selection toolbar then offers **Forward**.
- In message view, each row has a checkbox, so text-only and file-bearing messages can be selected together. Right-clicking a message row also forwards that single message.
- The destination dialog lists only **groups or people the current Telegram account can actually write to**. Read-only conversations and broadcast channels are omitted, and destinations can be searched by name.
- Forwarding uses Telegram's native `forwardMessages`; the server forwards the message directly, so omy **does not download the file and upload it again**. Message IDs are deduplicated and sent in original chronological order.
- One operation accepts at most 100 messages, keeping the action to one server request and avoiding a partially completed multi-batch operation that cannot be retried safely.
- The destination dialog can **create a supergroup whose only initial member is you**. The new group is selected after creation, but nothing is sent until you press **Forward here**.
- Forwarding stays within the same Telegram account. A source conversation with protected content cannot be forwarded, and omy reports that before sending the request.

Search comes in two forms, and the interface keeps them clearly apart:

- **Filter locally**: narrows the entries already listed. The search term is not sent anywhere.
- **Search on the server**: Telegram searches server-side and finds history that was never listed. **This sends your search term to Telegram**, so it takes an explicit switch, after which a notice states exactly which term was sent.

Searching on the server from the conversation list searches across all conversations. Conversations you lack access to are skipped silently, but rate limiting and network errors are reported as such — that is a different thing from "nothing was found".

### Uploading

When uploading into a writable conversation, omy always sends **as a file** rather than letting the client classify by content. Sending as a photo makes the server re-encode the data, which is fatal for ciphertext — and the resulting failure points at the key rather than at the upload method.

### Current limitations

- Some groups enable "restrict saving content" (`noforwards`). omy can still read and play those files normally, but **cannot forward** them. The Forward action is disabled, and the backend rejects the operation again before sending a request.
- A few files are served via a CDN redirect, which this version does not support. It reports a clear error instead of failing silently.
- **The message view for broadcast channels is read-only, and omy does not implement sponsored messages.** Telegram's terms require clients that display a message stream to support and faithfully display sponsored messages (ads), and carrying ad delivery and impression reporting inside a local encrypted file manager conflicts with what omy is for. omy's choice is to **browse channel messages read-only and not implement sponsored messages** (i.e. not participate in ad delivery), rather than refusing to show messages at all. Files and the media tabs in those channels work as usual.

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

### Keeping files permanently

The cache above is evictable: when space runs short, the least recently used blocks go first. If you want a particular file to open reliably offline, choose "Keep permanently" in its context menu.

- Permanently kept files are **outside the cache limit above and never evicted**, which is why Settings shows the two numbers separately: the temporary cache gets "used / limit", while permanent storage reports only its size and file count — it has no limit, and a progress bar would send you looking for a denominator that does not exist;
- Keeping a file is a real download: it fetches the file's ciphertext in full rather than just flagging it. A flag alone would make "permanent" a promise rather than a fact, and opening the file offline would still fail;
- Stopping keeps deletes those ciphertext blocks and gives the space back.

Settings → Remote locations → Ciphertext cache → "Manage permanent files…" lists everything kept permanently so you can release them one by one. Without that list your only option would be to go back to the location the file came from and find it in the directory again — and on Telegram that message may be long out of reach.

The list keeps working **after the location itself has been removed**: such a row reads "Removed location" but can still be released to reclaim the space.

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

::: warning Windows: Credential Manager can be filled up by other programs
If Settings reports that passwords cannot be stored securely and you are on
Windows, Credential Manager is most likely full. Some programs (VS Code's
Microsoft account tokens, observed in practice) keep appending credentials and
never clean them up; after a few hundred entries nothing new can be written.

First see who is using the space:

```powershell
(cmdkey /list | Out-String) -split "`r?`n" |
  Where-Object { $_ -match 'Target:' } |
  ForEach-Object { ($_ -replace '.*Target:\s*','') -split '[|:/]' | Select-Object -First 1 } |
  Group-Object | Sort-Object Count -Descending | Select-Object -First 5
```

Once you have confirmed a group is safe to remove (deleting sign-in tokens just
means signing in again; no data is lost), clear it by keyword:

```powershell
(cmdkey /list | Out-String) -split "`r?`n" |
  Where-Object { $_ -match 'keyword' } |
  ForEach-Object { (($_ -replace '.*Target:\s*','').Trim()) -replace '^LegacyGeneric:target=','' } |
  ForEach-Object { cmdkey /delete:$_ }
```

The target name must have the `LegacyGeneric:target=` prefix stripped; deleting
with the prefix reports "Element not found".
:::

The **name, address and username are stored in plain text**; only the password
is encrypted. This is deliberate: without the key those locations must still be
listed so you can be told to log in again, rather than showing an empty list
that looks like lost configuration. The cost is that anyone reading the file can
tell which server you use.

## Virtual remote locations (collections)

In the "Remotes" area you can create a **virtual remote location**: it connects to
no server and only collects **references** to files you pick from real remotes
(Telegram / WebDAV), letting you organize what you watch often across accounts and
chats.

- **Create**: in the sidebar "Remotes" area click "New virtual remote" and give it a name.
- **Add content**: right-click a file in a real remote (or select several) →
  "Add to virtual remote…". In the dialog, clicking a virtual remote's name adds
  to its **root** (files can live at the root — no folder needed first); click the
  arrow on the right to expand it and pick a specific subfolder.
- **Organize**: inside a virtual place the toolbar has "New folder / Paste";
  right-click a reference or folder to create, rename, delete, and **cut / copy /
  paste** (Ctrl+X / Ctrl+C / Ctrl+V and Delete work too). Cut/copy can only be
  pasted **between virtual places** — real remotes don't accept references; cut
  moves within one place, while copy and cross-place paste create new references.
  Deleting removes only the bookmark, **never** the real file. You **cannot upload
  files** into a virtual place — it only stores references.
- **Multi-select**: on desktop hold Ctrl (⌘ on macOS) and click items, or just
  click while a selection is active; on touch, **long-press** an item to enter
  selection mode (the local file list supports long-press multi-select too). A
  bulk "Cut / Copy / Delete" bar appears at the top; Ctrl+X / Ctrl+C / Delete
  work as well.
- **Manage from the sidebar**: **right-click** a virtual place in the sidebar to
  rename, delete, or **encrypt / unlock / lock** it. What gets encrypted is the
  collection itself (which files you point to); the name stays visible. It is
  sealed on disk with a standalone password + Argon2, and after locking no
  plaintext stays in memory — re-entering asks for the password. A virtual place
  password shares the **same session key pool** as ordinary .omy files: with the
  same password, unlocking one unlocks the other automatically, and it counts
  toward the "N passwords unlocked" indicator. omy remembers every encrypted
  vault it encounters while browsing or scanning (only the public salt and Argon2
  parameters from the file header — never the password), so once a vault has
  appeared anywhere, unlocking anything with the same password unlocks it too,
  even if you are not currently viewing that folder.
- **Open**: double-clicking a reference **previews/plays** the real file it points
  at in place (reading is delegated to the source, without leaving the virtual
  location); anything the real location already cached **opens instantly** (a
  reference shares the same cache as the real file, nothing is downloaded twice).
- **Locate the source**: right-click a reference —
  - for a Telegram file there are two entries: "Go to source message" (jumps to
    that chat's message timeline and highlights the message) and "Go to source
    file" (switches to the tab that holds the file and scrolls its card into view
    with a highlight; the file's type/tab is remembered when the reference is added);
  - for a non-Telegram source such as WebDAV there is just "Go to real location",
    equivalent to opening that file in its source folder.
- **Source changes**: a reference records the account's stable identity (not the
  local number that can change). So if you remove a remote from the list and add it
  back later, the reference re-attaches and works again; while the source is
  temporarily gone the reference does not disappear but is marked "source
  unavailable"; if the source is encrypted and still locked, it prompts you to unlock
  it first.

## Current limitations (first iteration)

- New folder and rename/delete are not available yet (encrypted upload is). Decrypting an encrypted folder (container) from remote to local is not supported yet either.
- No private cloud drivers beyond WebDAV and Telegram yet.
- No "unlock with fingerprint / face" tier for credential protection yet.

The design and progress notes live in `docs/research/14-remote-locations-cloud.md` in the repository; the Telegram part is in `docs/research/15-telegram-remote.md`.
