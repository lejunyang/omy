# omy

跨平台加密文件管理应用。**加密之后依然能直接用**——视频可任意拖动播放，图片音频文本可内嵌预览，文件列表显示解密后的真实文件名与缩略图。

Rust + Tauri v2，覆盖 Windows / macOS / Linux / Android / iOS。

> **当前状态**：设计阶段。设计文档已定稿，尚未开始实现。

---

## 核心特性

| | |
|---|---|
| **加密后可直接播放** | 视频任意 seek，无需先解密落盘；分块 AEAD + HTTP Range |
| **文件名加密** | 可选保留后缀；列表内显示解密后的原名 |
| **多密码自动扫描** | 一次输入多个密码，匹配的文件自动"显形" |
| **加密缩略图** | 存在文件头内，列表页只读头部即可显示 |
| **分片** | 手动指定大小，含冗余文件头；缺片可降级播放 |
| **局域网共享** | 只传密文、只读；密钥不出本机 |
| **完整还原** | 默认 bit-for-bit 还原原始文件 |
| **CLI** | 与 GUI 共享同一核心库，行为一致 |

自定义格式扩展名为 `.omy`，通过 8 字节文件头 magic `"OMYFILE" + 0x01` 识别。

---

## 文档

完整技术设计见 **[docs/research/](docs/research/)**，从 [README](docs/research/README.md) 进入。

| 主题 | 文档 |
|---|---|
| 需求与威胁模型 | [01](docs/research/01-requirements-and-threat-model.md) |
| 文件格式规范 | [02](docs/research/02-file-format-spec.md) |
| 密钥体系 | [03](docs/research/03-key-management.md) |
| 媒体播放架构 | [04](docs/research/04-media-playback.md) |
| 文件夹与分片 | [05](docs/research/05-container-and-sharding.md) |
| 局域网共享 | [06](docs/research/06-lan-sharing.md) |
| 平台适配与旁路防护 | [07](docs/research/07-platform-and-sidechannels.md) |
| UI/UX 设计 | [08](docs/research/08-ui-ux-design.md) |
| CLI 设计 | [09](docs/research/09-cli-design.md) |
| 许可证与专利 | [10](docs/research/10-licensing-and-patents.md) |
| 工程实现路线 | [11](docs/research/11-engineering-roadmap.md) |
| 设计决策记录 | [12](docs/research/12-decision-log.md) |

**可交互界面原型**：[docs/research/appendix/ui-prototype.html](docs/research/appendix/ui-prototype.html)（浏览器直接打开）

---

## 参考实现与验证

`docs/research/reference/` 下有一份 Python 参考实现，用于验证格式设计的可实现性与自洽性。

```bash
# 准备环境
python -m venv .venv
.venv\Scripts\python.exe -m pip install cryptography argon2-cffi zstandard   # Windows
# .venv/bin/python -m pip install cryptography argon2-cffi zstandard          # Unix

cd docs/research

# 参考实现自测（68 项）
../../.venv/Scripts/python.exe reference/test_omy.py

# 文档 ↔ 实现交叉验证（98 项）
../../.venv/Scripts/python.exe verify_spec.py

# 重新生成测试向量（修改 omy_ref.py 后必须执行）
../../.venv/Scripts/python.exe reference/gen_vectors.py
```

当前状态：**68 + 98 全部通过**。详见[验证报告](docs/research/appendix/verification-report.md)。

> ⚠️ 修改 `reference/omy_ref.py` 后必须重跑 `gen_vectors.py`，再跑上述两个测试回归。

---

## 计划中的许可结构

分层授权，让核心格式库能被任何人集成：

| 组件 | 许可证 |
|---|---|
| `omy-core`（格式 + 加密，零 FFmpeg 依赖） | MIT OR Apache-2.0 |
| `omy-media`（FFmpeg 封装） | LGPL-2.1+ |
| `omy-cli` / `omy-gui` | GPL-3.0 |
| 格式规范文档 | CC BY 4.0 |

详见 [10-licensing-and-patents.md](docs/research/10-licensing-and-patents.md)。

---

## 安全说明

**威胁模型**：防止文件被直接访问和识别内容。接受"文件可被识别为加密文件"这一前提。

**不提供**：隐藏卷级别的可否认性、安全擦除、抗内存取证、抗恶意软件。这些局限在设计文档中有明确说明，请勿超出其边界使用。

密码遗忘将导致数据永久丢失——**设计上不存在后门或找回机制**。
