---
title: Graphical interface
---

# Graphical interface

`omy-gui` is the desktop and mobile interface, built on Tauri v2 and Vue 3.

## It works like a file manager

The interface is a file browser rather than an "encryption tool": a location sidebar on the left, a list of entries on the right, double-click to enter or open (single tap on mobile).

**No password is needed to get in.** Opening the interface shows an ordinary browsing view, with `.omy` files marked as locked. You enter a password only when you want to see a file's contents, and other files in the same vault unlock automatically afterwards.

Unlocked entries show the **decrypted original filename and thumbnail**, with a playback tier badge (⚡ / 🔄 / 🐌) in the corner, so it is immediately clear whether a video will play smoothly.

## Inline preview and playback

Opening an unlocked media file previews it in place:

- **Video**: scrub the timeline with a preview of the target frame, adjust volume and playback speed, or jump 10 seconds backward/forward. On mobile, swipe to seek with a frame preview and hold for 2× playback. Timeline thumbnails are generated in memory on demand; nothing is decrypted to disk first
- **Images, audio, text**: displayed inline

Everything travels over the custom `omystream://` protocol, so plaintext is never written to disk. The security precondition for this path was established by measurement: the response headers must include `Cache-Control: no-store` and friends, or the WebView may cache decrypted data on disk.

## Main features

| Feature | Notes |
|---|---|
| Encrypt | Pick files or folders; set chunk size, KDF profile, thumbnails, original handling |
| Decrypt / restore | Restore one or many to a chosen location |
| Browse | Mixed listing of plain files and `.omy`, with lock state visible |
| Preview and playback | Video seeking, volume, speed, 10-second jumps and mobile press-to-speed-up; images, audio and text inline |
| Password management | Add key slots, change passwords, re-encrypt |
| Context menu | Open, reveal in file manager, password management, move to trash |
| Devices and sharing | Discover, pair, browse remote files and play them directly |
| Remote locations | Connect a WebDAV cloud / NAS and stream `.omy`; on Android, open externally as read-only or editable; see [Remote locations](remote-locations) |
| Settings | General, Remote locations, Security & passwords, Encryption defaults, Playback & preview, Devices & sharing, About (see below) |
| Theme | Light and dark (under Settings → General) |
| Languages | Simplified Chinese and English, following the system locale (under Settings → General) |

## Security constraints

The interface runs under a strict CSP: `default-src 'none'` and `script-src 'self'`, with no `eval` and no inline scripts.

Two consequences follow. Vue templates must be **precompiled** (the runtime compiler uses `new Function` internally, which the CSP blocks), and scripts or styles cannot execute from data URIs. `img-src` separately allows data URIs for in-memory images such as timeline frame previews.

Tauri's `protocol-asset` is also **not enabled** — that protocol exposes filesystem paths directly, which contradicts keeping plaintext off disk. All content goes through the controlled `omystream://` instead.

The native directory picker is invoked only on the Rust side and wrapped in a controlled command, so the frontend has no ability to select arbitrary paths (the capabilities file grants no dialog permission).

## Mobile

The Android build shares the same components and only swaps the shell — it is not a second interface. List, search, progress and statistics logic are identical.

The breakpoint is 768px: below it a single column with bottom navigation, above it the sidebar layout. Rotating the device switches in real time. On mobile the bottom navigation has four tabs — **Files / Remote / Devices / Settings** — with language and theme moved into **Settings → General**. The shared top bar on the Files and Remote locations pages stays below the system status bar; when Android WebView exposes no safe-area inset, it falls back to 24 px.

Opening a video preview on Android automatically switches to landscape and closing it restores the system orientation. The player keeps its touch gesture model even when the landscape viewport exceeds the responsive breakpoint. The system Back button first closes the active menu or dialog, returns from a settings subpage, closes the preview, or moves up one folder; it exits the app only from the application start state. File menus use a centered modal on mobile, and Telegram login omits the desktop-only tdata reuse entry.

::: tip Touch interaction genuinely differs
Desktop opens entries by double-click, a gesture touchscreens do not have (`dblclick` in a mobile WebView either never fires or gets eaten by double-tap zoom). So opening, multi-select and dragging are handled separately on mobile — it is not merely CSS adaptation.
:::

Two platform limits apply on mobile: no FFmpeg, so video thumbnails and P2/P3 playback are unavailable; and no trash, so "move to trash" is not offered.

### Storage access permission

On first launch the sidebar shows "Allow access to phone files". Only after granting it does the sidebar list locations such as internal storage, SD card, camera, documents, movies and music.

This uses Android's all-files access (`MANAGE_EXTERNAL_STORAGE`). On API 30 and above it opens the system settings page; on API 29 and below it is the standard runtime permission dialog.

::: tip Why not the system file picker (SAF)?
SAF hands out `content://` URIs. After a rename the URI changes and the granted permission stops working — which breaks exactly the precondition encrypted writes rely on, atomic replacement within the same directory. All-files access keeps real file paths.
:::

The permission state is queried live every time, never cached in the app. That is deliberate: a cached "granted" flag would keep claiming access after the user revokes it in system settings, and every file operation would then fail with a permission error whose message points nowhere near the real cause. When access is revoked the sidebar reports an error instead of returning a partial listing.

## Settings

On desktop settings is a dialog (gear in the top bar); on mobile it is a list with second-level pages under the **Settings** tab. The categories are the same:

| Category | Contents |
|---|---|
| General | Language, theme, default view (grid / list), open at startup (last folder / home) |
| Remote locations | Recognition scope, request concurrency, and the **Ciphertext cache** second-level page (limit, usage, clear now, clear on exit) |
| Security & passwords | Auto-lock (idle duration, whether backgrounding counts as idle, playback exemption), number of passwords loaded in the current session, lock now, recovery-code hint |
| Encryption defaults | KDF profile, chunk size, pre-encryption compression, filename encryption, original-file handling; these become the initial values in the encrypt dialog |
| Playback & preview | Whether to show thumbnails in the list (thumbnails live in the file header, so showing them needs no body read) |
| Devices & sharing | This device's name, whether LAN sharing auto-starts, entry to manage paired devices |
| About | App version, OMYFILE format version, layered open-source licenses, config path and portable mode |

The default view and startup location are persisted: toggling grid/list in the UI is remembered, and the last browsed folder is restored on the next launch.

Auto-lock semantics: backgrounding counts as **idle** (whether it locks depends on the idle setting), active media playback is not idle, and you can always **Lock now** from the security page.

## Launching

Desktop:

```bash
cargo run --release -p omy-gui
```

Android: install the APK from [Releases](https://github.com/lejunyang/omy/releases).

Full build steps are in [Building from source](/en/guide/building).
