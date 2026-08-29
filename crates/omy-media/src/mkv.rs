//! Matroska / WebM 的 EBML 结构解析。
//!
//! # 为什么需要这个模块
//!
//! P2（转封装）播放要能 seek。文档 §6.2 给出的方案是：
//!
//! ```text
//! 播放器 seek 到 t 秒
//!   → 查 Cues 索引，定位覆盖 t 的 Cluster
//!   → 算出这些 Cluster 在原文件的字节区间
//!   → 用分块 AEAD 解密对应区间（复用与 P1 完全相同的逻辑）
//!   → 只把这段字节喂给 FFmpeg → fMP4 片段
//! ```
//!
//! 这个绕法是**必需的**，不是优化。Spike 实测确认：
//!
//! - FFmpeg 的**输入 seek**（`-ss` 在 `-i` 前）在管道输入下失效，
//!   报 `Seek to desired resync point failed`，产物只有 `ftyp`+`moov`
//!   而无 `moof`——因为管道不能 seek。
//! - **输出 seek**（`-ss` 在 `-i` 后）可用，但要从头解完整个流才能到达
//!   目标时间点。对两小时的电影 seek 到 1:50:00 意味着解 110 分钟数据。
//!
//! 而只喂目标区间就没有这个问题。Spike 进一步确认了两件事：
//!
//! - 裸 Cluster（不带文件头）**无法** demux：`Invalid data found`。
//!   因为解码器需要 Tracks 元素才知道每条轨道的编码参数。
//! - **文件头 + 任意中间 Cluster** 拼接后**可以** demux，实测用
//!   754 字节头部 + 第 30 个 Cluster 成功产出可解码的 fMP4。
//!
//! 所以本模块要提供两样东西：**头部字节范围**与 **Cluster 索引**。
//!
//! # EBML 简述
//!
//! Matroska 基于 EBML，结构是嵌套的 `(ID, Size, Data)`：
//!
//! - **ID**：1–4 字节，首字节的前导 1 的个数决定总长度（含标记位）
//! - **Size**：1–8 字节，VINT 编码，首字节前导 0 的个数决定长度（不含标记位）
//! - **Data**：Size 指定的字节数；容器类元素的 Data 又是若干子元素
//!
//! 注意 ID 与 Size 的编码规则**不同**：ID 保留标记位（便于直接比较），
//! Size 去掉标记位（表示真实数值）。这是最容易写错的地方。

use crate::error::{MediaError, Result};

/// EBML 头（整个文件的第一个元素）。
const ID_EBML: u32 = 0x1A45_DFA3;
/// Segment：除 EBML 头外的所有内容都在这里面。
const ID_SEGMENT: u32 = 0x1853_8067;
/// Cluster：实际的媒体数据块，seek 的落点。
const ID_CLUSTER: u32 = 0x1F43_B675;
/// Tracks：轨道定义，解码器必需。
const ID_TRACKS: u32 = 0x1654_AE6B;
/// Cues：时间→位置索引。
const ID_CUES: u32 = 0x1C53_BB6B;
/// SegmentInfo：时长、TimestampScale 等。
const ID_INFO: u32 = 0x1549_A966;
/// Cluster 内的时间戳（相对 Segment 起点，单位为 TimestampScale）。
const ID_TIMESTAMP: u32 = 0x00E7;
/// TimestampScale：把 Cluster 时间戳换算成纳秒的乘数。
const ID_TIMESTAMP_SCALE: u32 = 0x002A_D7B1;
/// Duration：Segment 总时长（浮点，单位为 TimestampScale）。
const ID_DURATION: u32 = 0x4489;

/// 扫描的元素数量上限。
///
/// 恶意文件可以构造出天文数字的空元素来拖死解析。
/// 一部正常的电影 Cluster 数量在数千级别，这个上限足够宽松。
const MAX_ELEMENTS: usize = 200_000;

/// 头部允许的最大字节数。
///
/// 头部要整段拼进每个 remux 请求，太大就失去了"只喂小头部"的意义。
/// 正常 MKV 的头部（EBML+Info+Tracks）通常几百字节到几 KB；
/// 带大量附件（如内嵌字体）的可能到几百 KB。超过就说明这个文件
/// 不适合走"拼接 seek"路径，应当降级。
const MAX_HEADER_BYTES: u64 = 4 * 1024 * 1024;

/// 一个 EBML 元素的位置与大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element {
    /// 元素 ID（含标记位，便于与常量直接比较）。
    pub id: u32,
    /// 元素在文件中的起始偏移（指向 ID 的第一个字节）。
    pub offset: u64,
    /// 从 `offset` 到数据末尾的总字节数（ID + Size + Data）。
    pub total_len: u64,
    /// 数据部分的起始偏移。
    pub data_offset: u64,
    /// 数据部分的字节数。
    pub data_len: u64,
}

impl Element {
    /// 数据部分的结束偏移（不含）。
    #[must_use]
    pub const fn data_end(&self) -> u64 {
        self.data_offset + self.data_len
    }

    /// 整个元素的结束偏移（不含）。
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset + self.total_len
    }
}

/// 一个 Cluster 的索引条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterEntry {
    /// 在文件中的起始偏移。
    pub offset: u64,
    /// 总字节数（ID + Size + Data）。
    pub len: u64,
    /// Cluster 的时间戳，已换算为毫秒。
    ///
    /// `None` 表示这个 Cluster 没有 Timestamp 子元素——
    /// 规范要求必须有，但损坏文件可能缺失。
    pub timestamp_ms: Option<u64>,
}

impl ClusterEntry {
    /// 结束偏移（不含）。
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset + self.len
    }
}

/// MKV 文件的结构索引。
#[derive(Debug, Clone, Default)]
pub struct MkvIndex {
    /// 头部字节范围 `[0, header_len)`。
    ///
    /// 包含 EBML 头、Segment 头、SegmentInfo、Tracks 等所有
    /// **第一个 Cluster 之前**的内容。remux 任意区间时都要带上它。
    pub header_len: u64,
    /// 所有 Cluster，按文件顺序。
    pub clusters: Vec<ClusterEntry>,
    /// TimestampScale（纳秒）。默认 1_000_000，即 Cluster 时间戳单位为毫秒。
    pub timestamp_scale: u64,
    /// Segment 总时长（毫秒），来自 SegmentInfo。
    pub duration_ms: Option<u64>,
    /// 是否找到了 Tracks 元素。
    ///
    /// 没有 Tracks 的文件无法确定解码参数，拼接 seek 必然失败，
    /// 调用方应据此降级。
    pub has_tracks: bool,
    /// 是否存在 Cues 索引。
    ///
    /// 仅作参考：本实现直接扫 Cluster 建索引，不依赖 Cues。
    /// Cues 可能缺失或不准（很多工具生成的 MKV 没有 Cues）。
    pub has_cues: bool,
}

impl MkvIndex {
    /// 找出覆盖 `t` 毫秒的 Cluster 下标。
    ///
    /// 返回**起始时间戳不晚于 `t`** 的最后一个 Cluster——
    /// 也就是播放 `t` 时刻需要的那一个。
    ///
    /// # 为什么不能返回"最接近 t"的
    ///
    /// 若返回起始时间**晚于** `t` 的 Cluster，播放器会跳过 `t` 到该
    /// Cluster 之间的内容，表现为 seek 后画面往后跳了一截。
    /// 必须向前取整，宁可多解一点也不能少。
    #[must_use]
    pub fn cluster_for_time(&self, t_ms: u64) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, c) in self.clusters.iter().enumerate() {
            match c.timestamp_ms {
                Some(ts) if ts <= t_ms => best = Some(i),
                // 时间戳单调递增，一旦超过就不必再看
                Some(_) => break,
                // 缺时间戳的 Cluster 无法判断，跳过但不中断
                None => {}
            }
        }
        // 全都没有时间戳或全都晚于 t：退回第一个，
        // 让播放器从头开始也好过什么都不返回
        best.or(if self.clusters.is_empty() {
            None
        } else {
            Some(0)
        })
    }

    /// 取从 `start_idx` 开始、累计时长至少 `min_ms` 的 Cluster 字节区间。
    ///
    /// 返回 `(offset, len)`，是**原文件**中的绝对区间。
    /// 调用方解密这段字节，拼上 `[0, header_len)` 就能喂给 FFmpeg。
    ///
    /// `min_ms` 为 0 时只取一个 Cluster。
    #[must_use]
    pub fn cluster_range(&self, start_idx: usize, min_ms: u64) -> Option<(u64, u64)> {
        let first = self.clusters.get(start_idx)?;
        let start_ts = first.timestamp_ms;
        let mut end = first.end();

        for c in self.clusters.iter().skip(start_idx + 1) {
            // 已经攒够时长就停
            if let (Some(s), Some(cur)) = (start_ts, c.timestamp_ms) {
                if cur.saturating_sub(s) >= min_ms {
                    break;
                }
            }
            end = c.end();
        }
        Some((first.offset, end - first.offset))
    }
}

/// 读一个 EBML ID。
///
/// 返回 `(id, 占用字节数)`。ID **保留标记位**，这样 `0x1F43B675`
/// 这样的常量可以直接比较，不必再还原。
fn read_id(data: &[u8], pos: usize) -> Option<(u32, usize)> {
    let first = *data.get(pos)?;
    if first == 0 {
        // 前导 1 的个数超过 4，不是合法 ID
        return None;
    }
    // 首字节最高位起第一个 1 的位置决定总长度
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let mut id: u32 = 0;
    for i in 0..len {
        id = (id << 8) | u32::from(*data.get(pos + i)?);
    }
    Some((id, len))
}

/// 读一个 EBML 数据大小（VINT）。
///
/// 返回 `(size, 占用字节数)`。`size` 为 `None` 表示**未知长度**
///（所有数据位全为 1），这在流式 Matroska 里合法，表示"读到下一个
/// 同级元素为止"。
///
/// 与 ID 不同，这里**去掉标记位**，因为要的是真实数值。
fn read_size(data: &[u8], pos: usize) -> Option<(Option<u64>, usize)> {
    let first = *data.get(pos)?;
    if first == 0 {
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    // 去掉标记位：首字节只保留标记位之后的数据位。
    //
    // len == 8 时首字节是 0x01，标记位占满整个字节，数据位为 0。
    // 此时 `0xFF >> 8` 会**移位溢出 panic**（Rust 的移位量必须小于位宽），
    // 必须用 checked_shr 表达"移满即为 0"。这是 8 字节 VINT 的必经之路，
    // 而 8 字节 VINT 在真实 MKV 里很常见（muxer 常用最大宽度占位）。
    let mask = 0xFFu8.checked_shr(u32::try_from(len).ok()?).unwrap_or(0);
    let mut size = u64::from(first & mask);
    let mut all_ones = (first & mask) == mask;
    for i in 1..len {
        let b = *data.get(pos + i)?;
        size = (size << 8) | u64::from(b);
        if b != 0xFF {
            all_ones = false;
        }
    }
    if all_ones {
        // 未知长度
        return Some((None, len));
    }
    Some((Some(size), len))
}

/// 在 `[from, to)` 范围内读出下一个元素。
fn read_element(data: &[u8], from: u64, to: u64) -> Option<Element> {
    let pos = usize::try_from(from).ok()?;
    let (id, id_len) = read_id(data, pos)?;
    let (size, size_len) = read_size(data, pos + id_len)?;
    let header = u64::try_from(id_len + size_len).ok()?;
    let data_offset = from + header;

    let data_len = match size {
        Some(s) => {
            // 越界的长度声明：截断到可用范围而非直接失败。
            // 截断的文件（缺片播放场景）很常见，不该因此完全解析不了。
            if data_offset.saturating_add(s) > to {
                to.saturating_sub(data_offset)
            } else {
                s
            }
        }
        // 未知长度：延伸到范围末尾
        None => to.saturating_sub(data_offset),
    };

    Some(Element {
        id,
        offset: from,
        total_len: header + data_len,
        data_offset,
        data_len,
    })
}

/// 读一个无符号整数型元素的值。
fn read_uint(data: &[u8], el: &Element) -> Option<u64> {
    let start = usize::try_from(el.data_offset).ok()?;
    let len = usize::try_from(el.data_len).ok()?;
    if len == 0 || len > 8 {
        return None;
    }
    let slice = data.get(start..start.checked_add(len)?)?;
    let mut v: u64 = 0;
    for b in slice {
        v = (v << 8) | u64::from(*b);
    }
    Some(v)
}

/// 读一个浮点型元素的值（4 或 8 字节）。
fn read_float(data: &[u8], el: &Element) -> Option<f64> {
    let start = usize::try_from(el.data_offset).ok()?;
    let len = usize::try_from(el.data_len).ok()?;
    let slice = data.get(start..start.checked_add(len)?)?;
    match len {
        4 => {
            let arr: [u8; 4] = slice.try_into().ok()?;
            Some(f64::from(f32::from_be_bytes(arr)))
        }
        8 => {
            let arr: [u8; 8] = slice.try_into().ok()?;
            Some(f64::from_be_bytes(arr))
        }
        _ => None,
    }
}

/// 判断数据看起来是否为 Matroska/WebM。
///
/// 只看 EBML 魔数，不做完整解析——用于快速排除非 MKV 文件。
#[must_use]
pub fn looks_like_mkv(data: &[u8]) -> bool {
    // 用切片比较而非逐个索引：短数据直接返回 None，不可能越界
    data.get(..4) == Some(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// 解析 MKV 结构，产出头部范围与 Cluster 索引。
///
/// # 为什么用正规 EBML 解析而不是扫字节
///
/// Spike 阶段用「扫 `1F 43 B6 75` 字节序列」定位 Cluster，那样足够验证
/// 可行性。但生产实现绝不能这么做：媒体数据里完全可能偶然出现这四个
/// 字节，误命中会切出一个非法区间，表现为随机的播放失败——
/// 而且极难复现和排查。
///
/// # Errors
///
/// 数据不是 Matroska、EBML 结构损坏到无法定位 Segment，
/// 或头部超过 [`MAX_HEADER_BYTES`] 时返回错误。
pub fn parse(data: &[u8]) -> Result<MkvIndex> {
    if !looks_like_mkv(data) {
        return Err(MediaError::InvalidInput {
            reason: "不是 Matroska/WebM 文件（缺少 EBML 魔数）".to_string(),
        });
    }
    let total = u64::try_from(data.len()).map_err(|_| MediaError::InvalidInput {
        reason: "文件过大".to_string(),
    })?;

    // 第一层：EBML 头 + Segment
    let ebml = read_element(data, 0, total).ok_or_else(|| MediaError::InvalidInput {
        reason: "无法解析 EBML 头".to_string(),
    })?;
    if ebml.id != ID_EBML {
        return Err(MediaError::InvalidInput {
            reason: format!("首个元素不是 EBML 头（id={:#X}）", ebml.id),
        });
    }

    // 找 Segment。EBML 头之后通常紧跟 Segment，
    // 但中间也可能有 Void 填充，所以要循环找。
    let mut pos = ebml.end();
    let mut segment: Option<Element> = None;
    let mut guard = 0usize;
    while pos < total && guard < 64 {
        guard += 1;
        let Some(el) = read_element(data, pos, total) else {
            break;
        };
        if el.id == ID_SEGMENT {
            segment = Some(el);
            break;
        }
        if el.total_len == 0 {
            break;
        }
        pos = el.end();
    }
    let segment = segment.ok_or_else(|| MediaError::InvalidInput {
        reason: "未找到 Segment 元素".to_string(),
    })?;

    let mut idx = MkvIndex {
        // 规范默认值：1 毫秒
        timestamp_scale: 1_000_000,
        ..MkvIndex::default()
    };

    // 第二层：遍历 Segment 的直接子元素
    let seg_end = segment.data_end().min(total);
    let mut p = segment.data_offset;
    let mut first_cluster: Option<u64> = None;
    let mut count = 0usize;
    let mut duration_raw: Option<f64> = None;

    while p < seg_end && count < MAX_ELEMENTS {
        count += 1;
        let Some(el) = read_element(data, p, seg_end) else {
            break;
        };
        // 长度为 0 的元素会导致死循环，必须防
        if el.total_len == 0 {
            break;
        }

        match el.id {
            ID_CLUSTER => {
                if first_cluster.is_none() {
                    first_cluster = Some(el.offset);
                }
                let ts = read_cluster_timestamp(data, &el);
                idx.clusters.push(ClusterEntry {
                    offset: el.offset,
                    len: el.total_len,
                    timestamp_ms: ts,
                });
            }
            ID_TRACKS => idx.has_tracks = true,
            ID_CUES => idx.has_cues = true,
            ID_INFO => {
                let (scale, dur) = read_segment_info(data, &el);
                if let Some(s) = scale {
                    idx.timestamp_scale = s;
                }
                duration_raw = dur;
            }
            _ => {}
        }
        p = el.end();
    }

    // Cluster 时间戳换算成毫秒需要 TimestampScale，
    // 而 SegmentInfo 可能出现在 Cluster **之后**（少见但合法）。
    // 所以先按原始值存，这里再统一换算。
    let scale_ms = idx.timestamp_scale as f64 / 1_000_000.0;
    if (scale_ms - 1.0).abs() > f64::EPSILON {
        for c in &mut idx.clusters {
            if let Some(ts) = c.timestamp_ms {
                // 原始值 × scale(ns) / 1e6 = 毫秒
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let ms = (ts as f64 * scale_ms) as u64;
                c.timestamp_ms = Some(ms);
            }
        }
    }
    if let Some(d) = duration_raw {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = (d * scale_ms) as u64;
        idx.duration_ms = Some(ms);
    }

    // 头部 = 第一个 Cluster 之前的一切。
    // 没有 Cluster 时整个文件都是头部（比如只有元数据的文件）。
    idx.header_len = first_cluster.unwrap_or(seg_end);

    if idx.header_len > MAX_HEADER_BYTES {
        return Err(MediaError::InvalidInput {
            reason: format!(
                "MKV 头部 {} 字节，超过上限 {}——不适合拼接式 seek",
                idx.header_len, MAX_HEADER_BYTES
            ),
        });
    }

    Ok(idx)
}

/// 读 Cluster 的 Timestamp 子元素（原始值，尚未乘 TimestampScale）。
fn read_cluster_timestamp(data: &[u8], cluster: &Element) -> Option<u64> {
    let end = cluster.data_end();
    let mut p = cluster.data_offset;
    let mut guard = 0usize;
    // Timestamp 按规范是 Cluster 的第一个子元素，
    // 但容错起见多扫几个（有些工具会先写 Void）
    while p < end && guard < 16 {
        guard += 1;
        let el = read_element(data, p, end)?;
        if el.total_len == 0 {
            return None;
        }
        if el.id == ID_TIMESTAMP {
            return read_uint(data, &el);
        }
        p = el.end();
    }
    None
}

/// 读 SegmentInfo 里的 TimestampScale 与 Duration。
fn read_segment_info(data: &[u8], info: &Element) -> (Option<u64>, Option<f64>) {
    let end = info.data_end();
    let mut p = info.data_offset;
    let mut scale = None;
    let mut dur = None;
    let mut guard = 0usize;
    while p < end && guard < 64 {
        guard += 1;
        let Some(el) = read_element(data, p, end) else {
            break;
        };
        if el.total_len == 0 {
            break;
        }
        match el.id {
            ID_TIMESTAMP_SCALE => scale = read_uint(data, &el),
            ID_DURATION => dur = read_float(data, &el),
            _ => {}
        }
        p = el.end();
    }
    (scale, dur)
}

/// 拼出可独立 demux 的 MKV 片段：文件头 + 指定字节区间。
///
/// 这是 P2 seek 的核心操作。Spike 已验证：裸 Cluster 无法 demux
///（缺 Tracks 元素，`Invalid data found`），而头部 + 任意中间 Cluster
/// 可以正常 demux 并 remux 成 fMP4。
///
/// `region` 应当是 [`MkvIndex::cluster_range`] 返回的区间对应的字节。
///
/// # Errors
///
/// `header_len` 超出数据范围时返回错误。
pub fn splice(data: &[u8], header_len: u64, region: &[u8]) -> Result<Vec<u8>> {
    let hl = usize::try_from(header_len).map_err(|_| MediaError::InvalidInput {
        reason: "头部长度超出范围".to_string(),
    })?;
    let head = data.get(..hl).ok_or_else(|| MediaError::InvalidInput {
        reason: format!("头部长度 {hl} 超出数据长度 {}", data.len()),
    })?;
    let mut out = Vec::with_capacity(hl + region.len());
    out.extend_from_slice(head);
    out.extend_from_slice(region);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 EBML VINT 大小字段。
    fn vint(v: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let marker = 1u8 << (8 - len);
        for i in (0..len).rev() {
            out[i] = u8::try_from((v >> (8 * (len - 1 - i))) & 0xFF).unwrap_or(0);
        }
        out[0] |= marker;
        out
    }

    /// 组一个元素：ID + Size + Data。
    fn el(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        // 按 ID 数值大小决定字节数
        if id > 0x00FF_FFFF {
            out.extend_from_slice(&id.to_be_bytes());
        } else if id > 0x0000_FFFF {
            out.extend_from_slice(&id.to_be_bytes()[1..]);
        } else if id > 0x0000_00FF {
            out.extend_from_slice(&id.to_be_bytes()[2..]);
        } else {
            out.push(u8::try_from(id).unwrap_or(0));
        }
        out.extend_from_slice(&vint(payload.len() as u64, 8));
        out.extend_from_slice(payload);
        out
    }

    fn uint_payload(v: u64) -> Vec<u8> {
        vec![
            u8::try_from((v >> 56) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 48) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 40) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 32) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 24) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 16) & 0xFF).unwrap_or(0),
            u8::try_from((v >> 8) & 0xFF).unwrap_or(0),
            u8::try_from(v & 0xFF).unwrap_or(0),
        ]
    }

    /// 造一个最小但结构完整的 MKV。
    fn make_mkv(cluster_times: &[u64]) -> Vec<u8> {
        let mut seg = Vec::new();
        // SegmentInfo：TimestampScale = 1ms
        let mut info = Vec::new();
        info.extend_from_slice(&el(ID_TIMESTAMP_SCALE, &uint_payload(1_000_000)));
        seg.extend_from_slice(&el(ID_INFO, &info));
        // Tracks（内容不重要，存在即可）
        seg.extend_from_slice(&el(ID_TRACKS, &[0x01, 0x02, 0x03]));
        // Clusters
        for t in cluster_times {
            let mut c = Vec::new();
            c.extend_from_slice(&el(ID_TIMESTAMP, &uint_payload(*t)));
            // 填充一些"媒体数据"
            c.extend_from_slice(&el(0x00A3, &[0xAA; 32]));
            seg.extend_from_slice(&el(ID_CLUSTER, &c));
        }

        let mut out = Vec::new();
        out.extend_from_slice(&el(ID_EBML, &[0x42, 0x86, 0x81, 0x01]));
        out.extend_from_slice(&el(ID_SEGMENT, &seg));
        out
    }

    #[test]
    fn parses_minimal_mkv() {
        let data = make_mkv(&[0, 1000, 2000]);
        let idx = parse(&data).expect("应能解析");
        assert_eq!(idx.clusters.len(), 3);
        assert!(idx.has_tracks, "应识别出 Tracks");
        assert_eq!(idx.timestamp_scale, 1_000_000);
        assert_eq!(idx.clusters[0].timestamp_ms, Some(0));
        assert_eq!(idx.clusters[1].timestamp_ms, Some(1000));
        assert_eq!(idx.clusters[2].timestamp_ms, Some(2000));
    }

    #[test]
    fn header_ends_at_first_cluster() {
        let data = make_mkv(&[0, 1000]);
        let idx = parse(&data).expect("应能解析");
        // 头部必须正好到第一个 Cluster 为止
        assert_eq!(
            idx.header_len, idx.clusters[0].offset,
            "头部应当止于第一个 Cluster"
        );
        assert!(idx.header_len > 0, "头部不能为空");
        // 头部必须包含 Tracks，否则拼接后无法 demux
        assert!(
            idx.header_len > 10,
            "头部应包含 EBML/Info/Tracks，不应过短"
        );
    }

    #[test]
    fn rejects_non_mkv() {
        assert!(parse(b"not a matroska file at all").is_err());
        assert!(parse(&[]).is_err());
        // MP4 的 ftyp 不该被当成 MKV
        assert!(parse(b"\x00\x00\x00\x20ftypisom").is_err());
    }

    #[test]
    fn looks_like_mkv_checks_magic() {
        assert!(looks_like_mkv(&[0x1A, 0x45, 0xDF, 0xA3, 0x00]));
        assert!(!looks_like_mkv(&[0x1A, 0x45, 0xDF]));
        assert!(!looks_like_mkv(b"RIFF"));
    }

    #[test]
    fn cluster_for_time_rounds_down() {
        let data = make_mkv(&[0, 1000, 2000, 3000]);
        let idx = parse(&data).expect("应能解析");

        // 正好命中
        assert_eq!(idx.cluster_for_time(1000), Some(1));
        // 落在中间：必须**向前**取整，否则 seek 会往后跳过内容
        assert_eq!(idx.cluster_for_time(1500), Some(1));
        assert_eq!(idx.cluster_for_time(2999), Some(2));
        // 超过末尾：取最后一个
        assert_eq!(idx.cluster_for_time(99_999), Some(3));
        // 起点
        assert_eq!(idx.cluster_for_time(0), Some(0));
    }

    #[test]
    fn cluster_range_covers_requested_duration() {
        let data = make_mkv(&[0, 1000, 2000, 3000, 4000]);
        let idx = parse(&data).expect("应能解析");

        // min_ms=0 只取一个
        let (off, len) = idx.cluster_range(1, 0).expect("应有区间");
        assert_eq!(off, idx.clusters[1].offset);
        assert_eq!(len, idx.clusters[1].len);

        // min_ms=2000 应当覆盖到 3000ms 那个 cluster
        let (off2, len2) = idx.cluster_range(1, 2000).expect("应有区间");
        assert_eq!(off2, idx.clusters[1].offset);
        let end = off2 + len2;
        assert!(
            end >= idx.clusters[3].offset,
            "2000ms 的请求应至少覆盖到第 3 个 cluster"
        );

        // 越界下标返回 None，而不是 panic
        assert!(idx.cluster_range(99, 0).is_none());
    }

    #[test]
    fn splice_produces_header_plus_region() {
        let data = make_mkv(&[0, 1000, 2000]);
        let idx = parse(&data).expect("应能解析");
        let (off, len) = idx.cluster_range(2, 0).expect("应有区间");

        let region = &data[usize::try_from(off).unwrap()..usize::try_from(off + len).unwrap()];
        let out = splice(&data, idx.header_len, region).expect("应能拼接");

        assert_eq!(out.len() as u64, idx.header_len + len);
        // 前半段必须与原文件头部逐字节相同
        assert_eq!(&out[..usize::try_from(idx.header_len).unwrap()],
                   &data[..usize::try_from(idx.header_len).unwrap()]);
        // 后半段必须是请求的区间
        assert_eq!(&out[usize::try_from(idx.header_len).unwrap()..], region);
        // 拼出来的东西自己也应当是合法 MKV 开头
        assert!(looks_like_mkv(&out));
    }

    #[test]
    fn splice_rejects_out_of_range_header() {
        let data = make_mkv(&[0]);
        assert!(splice(&data, 999_999, &[1, 2, 3]).is_err());
    }

    #[test]
    fn timestamp_scale_is_applied() {
        // 用非默认 scale：100 万纳秒 = 1ms 是默认，
        // 这里用 1000 万纳秒 = 10ms，Cluster 时间戳 100 应当变成 1000ms
        let mut seg = Vec::new();
        let mut info = Vec::new();
        info.extend_from_slice(&el(ID_TIMESTAMP_SCALE, &uint_payload(10_000_000)));
        seg.extend_from_slice(&el(ID_INFO, &info));
        seg.extend_from_slice(&el(ID_TRACKS, &[0x01]));
        let mut c = Vec::new();
        c.extend_from_slice(&el(ID_TIMESTAMP, &uint_payload(100)));
        c.extend_from_slice(&el(0x00A3, &[0xAA; 8]));
        seg.extend_from_slice(&el(ID_CLUSTER, &c));

        let mut data = Vec::new();
        data.extend_from_slice(&el(ID_EBML, &[0x42, 0x86, 0x81, 0x01]));
        data.extend_from_slice(&el(ID_SEGMENT, &seg));

        let idx = parse(&data).expect("应能解析");
        assert_eq!(idx.timestamp_scale, 10_000_000);
        assert_eq!(
            idx.clusters[0].timestamp_ms,
            Some(1000),
            "时间戳 100 × 10ms 应当等于 1000ms"
        );
    }

    #[test]
    fn vint_encoding_roundtrip() {
        // read_size 必须去掉标记位，read_id 必须保留——
        // 这两个规则搞反是 EBML 解析最经典的错误
        let buf = vint(1000, 8);
        let (size, len) = read_size(&buf, 0).expect("应能解析");
        assert_eq!(size, Some(1000));
        assert_eq!(len, 8);

        // 单字节 VINT：0x81 表示值 1
        let (s1, l1) = read_size(&[0x81], 0).expect("应能解析");
        assert_eq!(s1, Some(1));
        assert_eq!(l1, 1);

        // ID 保留标记位
        let (id, il) = read_id(&[0x1A, 0x45, 0xDF, 0xA3], 0).expect("应能解析");
        assert_eq!(id, ID_EBML);
        assert_eq!(il, 4);
    }

    #[test]
    fn unknown_size_does_not_hang() {
        // 未知长度（全 1）在流式 MKV 里合法，
        // 解析器必须能处理而不是死循环
        let (size, len) = read_size(&[0xFF], 0).expect("应能解析");
        assert_eq!(size, None, "0xFF 表示未知长度");
        assert_eq!(len, 1);
    }

    #[test]
    fn truncated_file_still_parses_available_clusters() {
        // 缺片播放场景：文件被截断，仍应能解析出可用部分
        let full = make_mkv(&[0, 1000, 2000, 3000]);
        let cut = &full[..full.len() * 2 / 3];
        // 不要求成功，但绝不能 panic 或死循环
        if let Ok(idx) = parse(cut) {
            assert!(
                idx.clusters.len() < 4,
                "截断后不应解析出全部 cluster"
            );
        }
    }

    #[test]
    fn zero_length_element_does_not_loop_forever() {
        // 恶意构造：声明长度为 0 的元素。
        // 若解析器不防，p 永远不前进 → 死循环挂死整个应用
        let mut data = Vec::new();
        data.extend_from_slice(&el(ID_EBML, &[0x42, 0x86, 0x81, 0x01]));
        // Segment 里塞一个长度为 0 的怪东西
        let mut seg = Vec::new();
        seg.push(0xA3);
        seg.push(0x80); // size = 0
        seg.extend_from_slice(&el(ID_TRACKS, &[0x01]));
        data.extend_from_slice(&el(ID_SEGMENT, &seg));

        // 关键是这行能返回——挂住就是失败
        let _ = parse(&data);
    }
}
