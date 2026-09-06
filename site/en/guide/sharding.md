---
title: Sharding
---

# Sharding

Split one large `.omy` into several smaller files, to work around single-file size limits in filesystems or cloud storage.

## Splitting

```bash
omy shard split --size 4095M big.omy
```

`--size` is required and has no default. Output files are numbered `.000`, `.001` and so on:

```
✓ 已切分为 3 片，每片上限 4.00 KiB，含冗余头

note.txt.omy.000    4152
note.txt.omy.001    4808
note.txt.omy.002    2862
```

Common sizes: `4095M` (the FAT32 single-file limit), `2000M` (some cloud services).

```bash
omy shard split --size 4095M --output-dir ./shards big.omy   # write elsewhere
omy shard split --size 4095M --remove-source big.omy         # delete the source afterwards
```

## Redundant headers

By default **every shard carries a copy of the file header**. That way losing the first shard still leaves the metadata (cipher, KDF parameters, key slots) recoverable from any other — otherwise losing shard zero would write off the entire dataset.

```bash
omy shard split --size 4095M --no-redundant-header big.omy
```

Turning it off saves a header's worth of space per shard, but reintroduces "lose the first shard, lose everything". Not recommended unless you have a great many shards and care intensely about size.

## Checking integrity

```bash
omy shard check shards/big.omy.001
```

```
分片数量    3 / 3
完整性      完整
冗余头      存在，丢失首片仍可恢复元信息
```

Pass any single shard; omy finds the rest of the group itself. When shards are missing it reports exactly which ones and the byte ranges they covered.

## Merging

```bash
omy shard merge shards/big.omy.000
```

## Degraded decryption

Sharding is not only about size limits — **missing shards remain usable**:

```bash
omy decrypt --ignore-missing-shards partial.omy.000
```

Because each chunk is encrypted independently, a missing shard only affects the chunks it covered. For video, a gap in the middle typically means a skipped section with everything before and after still playing.

Contrast this with encrypted archives, where a single corrupt spot often makes the whole thing unopenable.

::: warning Do not treat degradation as a backup strategy
Playback with missing shards is damage mitigation, not permission to deliberately keep fewer shards. Important data still needs complete backups.
:::

## Combining with container mode

The single large `.omy` produced by `--mode container` is often exactly what needs sharding:

```bash
omy encrypt photos/                          # → photos.omy
omy shard split --size 4095M photos.omy      # → photos.omy.000, .001, ...
```

The order matters: sharding operates on **already encrypted output** and is not part of encryption itself.
