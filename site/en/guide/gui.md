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

- **Video**: scrub the timeline freely, with nothing decrypted to disk first
- **Images, audio, text**: displayed inline

Everything travels over the custom `omystream://` protocol, so plaintext is never written to disk. The security precondition for this path was established by measurement: the response headers must include `Cache-Control: no-store` and friends, or the WebView may cache decrypted data on disk.

## Main features

| Feature | Notes |
|---|---|
| Encrypt | Pick files or folders; set chunk size, KDF profile, thumbnails, original handling |
| Decrypt / restore | Restore one or many to a chosen location |
| Browse | Mixed listing of plain files and `.omy`, with lock state visible |
| Preview and playback | Video seeking; images, audio and text inline |
| Password management | Add key slots, change passwords, re-encrypt |
| Context menu | Open, reveal in file manager, password management, move to trash |
| Devices and sharing | Discover, pair, browse remote files and play them directly |
| Theme | Light and dark |
| Languages | Simplified Chinese and English, following the system locale |

## Security constraints

The interface runs under a strict CSP: `default-src 'none'` and `script-src 'self'`, with no `eval` and no inline scripts.

Two consequences follow. Vue templates must be **precompiled** (the runtime compiler uses `new Function` internally, which the CSP blocks), and assets cannot be inlined as data URIs.

Tauri's `protocol-asset` is also **not enabled** — that protocol exposes filesystem paths directly, which contradicts keeping plaintext off disk. All content goes through the controlled `omystream://` instead.

The native directory picker is invoked only on the Rust side and wrapped in a controlled command, so the frontend has no ability to select arbitrary paths (the capabilities file grants no dialog permission).

## Mobile

The Android build shares the same components and only swaps the shell — it is not a second interface. List, search, progress and statistics logic are identical.

The breakpoint is 768px: below it a single column with bottom navigation, above it the sidebar layout. Rotating the device switches in real time.

::: tip Touch interaction genuinely differs
Desktop opens entries by double-click, a gesture touchscreens do not have (`dblclick` in a mobile WebView either never fires or gets eaten by double-tap zoom). So opening, multi-select and dragging are handled separately on mobile — it is not merely CSS adaptation.
:::

Two platform limits apply on mobile: no FFmpeg, so video thumbnails and P2/P3 playback are unavailable; and no trash, so "move to trash" is not offered.

## Launching

Desktop:

```bash
cargo run --release -p omy-gui
```

Android: install the APK from [Releases](https://github.com/lejunyang/omy/releases).

Full build steps are in [Building from source](/en/guide/building).
