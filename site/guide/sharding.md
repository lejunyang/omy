---
title: 分片
---

# 分片

把一个大的 `.omy` 切成多个小文件，用于绕过文件系统或网盘的单文件大小限制。

## 切分

```bash
omy shard split --size 4095M big.omy
```

`--size` 是必填的，没有默认值。产出的文件按 `原名.000`、`原名.001` 依次编号：

```
✓ 已切分为 3 片，每片上限 4.00 KiB，含冗余头

note.txt.omy.000    4152
note.txt.omy.001    4808
note.txt.omy.002    2862
```

常用尺寸：`4095M`（FAT32 单文件上限）、`2000M`（部分网盘限制）。

```bash
omy shard split --size 4095M --output-dir ./shards big.omy   # 输出到别处
omy shard split --size 4095M --remove-source big.omy         # 切完删原文件
```

## 冗余头

默认**每一片都带一份文件头**。这样即使丢了第一片，元信息（算法、KDF 参数、密钥槽）仍能从其他片恢复——否则丢首片就等于整份数据报废。

```bash
omy shard split --size 4095M --no-redundant-header big.omy
```

关掉能省一点空间（每片省一个文件头的大小），但把「丢首片 = 全废」的风险接了回来。除非片数极多且非常在意体积，不建议关。

## 检查完整性

```bash
omy shard check shards/big.omy.001
```

```
分片数量    3 / 3
完整性      完整
冗余头      存在，丢失首片仍可恢复元信息
```

任意传入一片即可，omy 会自己找同组的其他片。缺片时会明确报告缺哪几片以及对应的字节区间。

## 合并

```bash
omy shard merge shards/big.omy.000
```

## 缺片时的降级解密

分片的价值不只是绕过大小限制——**缺片仍可用**：

```bash
omy decrypt --ignore-missing-shards partial.omy.000
```

因为每块独立加密，缺失的片只影响它覆盖的那些块。对视频来说，缺中间一片通常表现为跳过一段，前后仍能正常播放；缺的不是首片时影响更小。

这与加密压缩包形成对比：后者往往一处损坏就整份打不开。

::: warning 别把降级当备份策略
缺片播放是损坏后的补救，不是设计给你故意少存几片用的。重要数据仍需完整备份。
:::

## 与容器模式配合

`--mode container` 打包出来的单个大 `.omy` 常常正是需要分片的对象：

```bash
omy encrypt photos/                          # → photos.omy
omy shard split --size 4095M photos.omy      # → photos.omy.000, .001, ...
```

顺序上必须先加密再分片：分片作用于**已加密的产物**，不参与加密过程本身。
