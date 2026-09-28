---
layout: home

hero:
  name: omy
  text: 加密之后依然能直接用
  tagline: 视频任意拖动播放，图片音频文本内嵌预览，列表里显示解密后的真实文件名与缩略图。跨平台，CLI 与图形界面共享同一套核心。
  actions:
    - theme: brand
      text: 快速上手
      link: /guide/getting-started
    - theme: alt
      text: omy 是什么
      link: /guide/what-is-omy
    - theme: alt
      text: GitHub
      link: https://github.com/lejunyang/omy

features:
  - title: 加密后可直接播放
    details: 分块 AEAD 加上 HTTP Range，视频能任意 seek，不需要先整份解密落盘。明文不写入磁盘。
  - title: 文件名也加密
    details: 可选保留后缀便于按类型筛选。列表里显示的是解密后的原名，而磁盘上是密文。
  - title: 多密码自动扫描
    details: 一次输入多个密码，目录里能解开的文件自动"显形"。单个文件最多 8 个独立密码槽。
  - title: 缩略图存在文件头里
    details: 列表页只读文件头就能显示缩略图，不必解密整个载荷。
  - title: 分片
    details: 手动指定每片大小，默认每片带冗余文件头。丢了首片仍能恢复元信息，缺片可降级播放。
  - title: 局域网共享
    details: 只传密文、只读。SPAKE2 配对加 Noise IK 信道，密钥不出本机。
  - title: 默认完整还原
    details: 解密结果与原始文件 bit-for-bit 一致，不是"看起来一样"。
  - title: CLI 是一等公民
    details: 13 个子命令，与图形界面共享同一个 omy-core，行为一致。支持 JSON 输出与精确退出码。
---

## 三条命令看清它做什么

```bash
# 加密一个视频。密码交互式输入，不回显，不进 shell history
omy encrypt holiday.mp4        # 产出 holiday.mp4.omy

# 不给密码也能看到文件的公开信息（算法、分块大小、KDF 参数）
omy info holiday.mp4.omy

# 解密还原。默认与原文件 bit-for-bit 一致
omy decrypt holiday.mp4.omy
```

## 项目状态

各 crate 的当前状态与测试规模：

| crate | 许可证 | 说明 |
|---|---|---|
| `omy-core` | MIT OR Apache-2.0 | 格式读写、密钥体系、分块 AEAD。零 FFmpeg 依赖 |
| `omy-media` | LGPL-2.1+ | 媒体探测、分级、缩略图。通过子进程调用 FFmpeg |
| `omy-net` | MIT OR Apache-2.0 | 设备发现、SPAKE2 配对、Noise IK 信道、密文块服务 |
| `omy-cli` | GPL-3.0+ | 命令行工具，13 个子命令 |
| `omy-gui` | GPL-3.0+ | Tauri v2 + Vue 3 图形界面 |

::: warning 平台验证范围
目前只在 **Windows** 与 **Android** 上实测过。Linux 与 macOS 的代码路径已实现，CI 会持续编译，并在每次发布时作为构建门禁（编不过就不发布），但**尚未在真机上运行过**。在这两个平台上使用请自行核实。
:::

::: danger 密码遗忘无法找回
设计上不存在后门或找回机制。密码遗忘即数据永久丢失。
:::
