---
title: Media preview and playback
---

# Media preview and playback

This is where omy differs most from ordinary encryption tools: an encrypted video can be **scrubbed freely** without decrypting the whole file to disk first.

## Why it works

The payload is encrypted in chunks (256 KiB by default), each with its own authentication tag, and chunk offsets are computable in O(1). When the player asks for the data at second 100, only those chunks get decrypted.

The GUI's player fetches data over the custom `omystream://` protocol, and the handler honours HTTP Range semantics, returning only the requested interval. Measured results: six out-of-order seeks (85% / 15% / 60% / 5% / 95% / 35%) all landed accurately with 0.00s deviation, taking 1–129 ms; the WebView was confirmed not to write decrypted data into its disk cache.

## Playback tiers

Not every format plays directly in a WebView. omy probes media information at encryption time and writes the verdict into the header, so the listing can show a badge:

| Tier | Meaning | How it plays |
|---|---|---|
| **P1** ⚡ | Direct | Custom protocol plus Range, no extra processing |
| **P2** 🔄 | Remux | FFmpeg changes the container without re-encoding → fMP4 → MSE |
| **P3** 🐌 | Full decode | FFmpeg decodes and transcodes → MSE, slowest |

Roughly: H.264 + AAC in MP4 is P1; the same codecs inside MKV is P2 (only the container needs changing); HEVC, AV1 or DTS are usually P3.

::: tip Tiers carry a reason and an alternative
A DTS + H.264 MKV lands in P3, but if it also has an AAC track, switching to it drops the file to P2. So the verdict includes the **reason** and the **alternative**, letting the interface guide you to another track instead of just saying "cannot play".
:::

The tiering is a **conservative cross-platform baseline**. Real capability is decided at runtime by `MediaSource.isTypeSupported()` — the same codec behaves differently across Chromium and WebKit and across versions, so hardcoding platform assumptions would rot with every browser release.

## Thumbnails

Generated at encryption time: images are scaled (pure Rust, no FFmpeg needed), videos have a frame grabbed (FFmpeg required). The result is encrypted into the header's TLV area.

The listing shows it by **reading the header only**, never touching the payload.

```bash
omy encrypt --thumbnail none private.mp4    # generate none at all
```

Videos default to the frame at 10% of the duration, avoiding black frames at the start. To choose yourself:

```bash
omy encrypt --thumbnail-frame 00:01:23 movie.mp4
omy encrypt --thumbnail-frame 83 movie.mp4       # seconds
omy encrypt --thumbnail-frame 83.5 movie.mp4
```

::: tip The GUI lets you pick while watching
With a single video selected, the encryption dialog offers "Video processing…". There you can play and scrub to choose the cover frame — the scrubber, the timecode and the picture stay in sync, so you never have to guess a timestamp.
:::

To confirm the frame you got, export it (WebP format, password required):

```bash
omy info --extract-thumbnail thumb.webp --with-password movie.mp4.omy
```

## Converting before encryption (GUI only)

The same "Video processing…" dialog can also convert the video **before** it is encrypted. It shows the source container, codecs, resolution, size and playback tier, then offers three choices:

| What to do | Time | Notes |
|---|---|---|
| Leave it alone | — | Encrypt as is; quality and size untouched |
| Remux | Seconds | Changes only the container (`-c copy`), never the video stream — no quality loss |
| Compress | Slow | Reduce resolution or bitrate; re-encodes |

Remuxing is mainly useful to turn MKV/MOV into MP4: most MP4s play directly (P1), which saves the on-the-fly remux at every playback.

::: warning Compression needs a full FFmpeg build
The build shipped on Windows is trimmed to probing, frame extraction and remuxing, and carries **no video encoder at all**, so the compress option is greyed out and the dialog says why. It becomes available automatically once you switch to a full FFmpeg build — see "Using your own FFmpeg" on the [installation page](/en/guide/install#ffmpeg).
:::

::: warning Some audio tracks are lost when remuxing
MP4 cannot hold PCM or DTS tracks; they have to be converted to AAC, and the bundled FFmpeg has no AAC encoder either. The dialog warns about this **before** you start, so you never end up with a silently soundless video. Choosing MKV, or switching to a full FFmpeg build, keeps the audio.

Conversion always drops subtitle tracks. Choose "leave it alone" to keep them.
:::

## Media metadata and moov cache

Two things are written by default and can be turned off:

```bash
omy encrypt --no-media-meta movie.mp4     # skip media metadata
omy encrypt --no-moov-cache movie.mp4     # skip caching the MP4 moov box
```

**Media metadata** (`TLV_MEDIA_META`) lets the listing know whether inline playback is possible without probing. Without it, every listing has to probe afresh.

**The moov cache** (`TLV_MOOV_CACHE`) is the MP4 index. A player must find it before starting, and it may live at the end of the file — caching it saves two seeks at startup. LAN playback and playback with missing shards benefit most.

Both trade a little header size for responsiveness; keep the defaults unless header size matters to you.

## Playing from the command line

The CLI has no built-in player, but it can pipe to an external one without plaintext landing on disk:

```bash
omy decrypt --stdout movie.mp4.omy | vlc -
omy decrypt --stdout movie.mp4.omy | mpv -
```

Playing this way usually **cannot seek** (pipes are not rewindable). Use the GUI for arbitrary scrubbing.

## Without FFmpeg

The Windows build ships with FFmpeg, so this describes the other platforms — or what happens once you delete the bundled copy:

- ✅ Encryption, decryption, sharding, sharing, image thumbnails — all fine
- ❌ Video thumbnails
- ❌ Media probing and tiering
- ❌ P2 / P3 playback (only P1 plays)

Check the current state with `omy doctor`. What is bundled and the lookup order are in [Installation](/en/guide/install#ffmpeg).
