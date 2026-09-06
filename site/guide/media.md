---
title: 媒体预览与播放
---

# 媒体预览与播放

这是 omy 与普通加密工具区别最大的地方：加密后的视频可以**任意拖动播放**，不需要先整份解密落盘。

## 为什么能做到

载荷按块加密（默认 256 KiB 一块），每块独立带认证标签，且块偏移可 O(1) 计算。播放器请求第 100 秒的数据时，只有对应的那几块被解密。

图形界面里的播放器通过自定义协议 `omystream://` 取数据，服务端按 HTTP Range 语义响应，只返回请求区间。实测数据：6 次乱序 seek（85% / 15% / 60% / 5% / 95% / 35%）全部落点准确，偏差 0.00 秒，耗时 1~129 ms；同时确认 WebView 没有把解密后的数据写进磁盘缓存。

## 播放路径分级

不是所有格式都能被 WebView 直接播放。omy 在加密时探测媒体信息，把判定结果写进文件头，列表页据此显示角标：

| 等级 | 含义 | 处理方式 |
|---|---|---|
| **P1** ⚡ | 直通 | 自定义协议 + Range，零额外处理 |
| **P2** 🔄 | 转封装 | FFmpeg 只改容器不重编码 → fMP4 → MSE |
| **P3** 🐌 | 全解码 | FFmpeg 解码转码 → MSE，最慢 |

大致规律：H.264 + AAC 的 MP4 是 P1；同样编码但装在 MKV 里是 P2（换个容器就行，不必重编码）；HEVC、AV1、DTS 之类通常是 P3。

::: tip 分级带原因和备选
一个 DTS + H.264 的 MKV 会被判成 P3，但如果它另有一条 AAC 音轨，切过去就能降到 P2。所以判定结果里除了等级还带**原因**和**备选方案**，界面据此引导你换轨道，而不是直接告诉你"播不了"。
:::

分级是**保守的跨平台基线**。真实能力由运行时的 `MediaSource.isTypeSupported()` 探测决定——同一编码在 Chromium 与 WebKit、乃至不同版本间表现不同，硬编码平台判断会随浏览器版本失效。

## 缩略图

加密时自动生成：图片是缩放原图（纯 Rust，不需要 FFmpeg），视频是抽帧（需要 FFmpeg）。加密后存进文件头的 TLV 区。

列表页**只读文件头**就能显示缩略图，不必接触载荷。

```bash
omy encrypt --thumbnail none private.mp4    # 完全不生成
```

视频默认取时长 10% 处的那一帧（避开片头黑帧）。要自己挑：

```bash
omy encrypt --thumbnail-frame 00:01:23 movie.mp4
omy encrypt --thumbnail-frame 83 movie.mp4       # 秒
omy encrypt --thumbnail-frame 83.5 movie.mp4
```

想确认取到的帧是否如预期，可以导出来看（格式是 WebP，需要密码）：

```bash
omy info --extract-thumbnail thumb.webp --with-password movie.mp4.omy
```

## 媒体元信息与 moov 缓存

默认会写入两样东西，都可以关掉：

```bash
omy encrypt --no-media-meta movie.mp4     # 不写媒体元信息
omy encrypt --no-moov-cache movie.mp4     # 不缓存 MP4 的 moov box
```

**媒体元信息**（`TLV_MEDIA_META`）让列表页无需解密探测就知道能否内嵌播放。关掉之后每次都得现探。

**moov 缓存**（`TLV_MOOV_CACHE`）是 MP4 的索引。播放器起播时要先找到它，而它可能在文件末尾——缓存之后起播能省掉两次 seek。局域网播放和缺片播放收益最大。

两者都属于「用一点体积换体验」，除非特别在意文件头大小，否则建议保持默认。

## 命令行里播放

CLI 不内置播放器，但可以管道给外部播放器，明文不落盘：

```bash
omy decrypt --stdout movie.mp4.omy | vlc -
omy decrypt --stdout movie.mp4.omy | mpv -
```

这种方式下播放器通常**不能 seek**（管道不可回退）。要任意拖动请用图形界面。

## 没有 FFmpeg 会怎样

FFmpeg 是可选的。缺了它：

- ✅ 加解密、分片、共享、图片缩略图 —— 全部正常
- ❌ 视频缩略图
- ❌ 媒体元信息探测与分级
- ❌ P2 / P3 播放（只有 P1 能播）

用 `omy doctor` 确认当前状态。安装与查找顺序见[安装](/guide/install#可选-ffmpeg)。
