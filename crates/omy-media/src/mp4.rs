//! MP4 box 树的最小解析：定位 `moov` 以便缓存进 TLV。
//!
//! # 为什么要这个
//!
//! 文档 §4.3 的优化：把 `moov` box 缓存进加密头部，起播时直接取用，
//! 连"跳到尾部读元数据"的那两次 Range 请求都省掉。对局域网远程播放
//! 收益最大，也顺带改善 iOS WKWebView 对 moov 在尾部的兼容性。
//!
//! # 为什么不用 ffprobe 做
//!
//! ffprobe 不输出 box 偏移。自己解析顶层 box 只需几十行，
//! 且**不需要**理解 box 内部结构——只要找到 `moov` 的起止位置。
//! 引入完整的 MP4 解析库反而扩大了攻击面。
//!
//! # 安全约束
//!
//! 本模块处理**不可信输入**，因此：
//! - 所有算术用 `checked_*`，绝不允许溢出后回绕导致越界；
//! - 遇到任何不一致立即停止，不尝试"修复"；
//! - 限制迭代次数，防止构造出的循环让解析卡死。

use crate::error::{MediaError, Result};

/// 顶层 box 的最大数量。
///
/// 正常 MP4 顶层只有个位数个 box。设上限防止恶意文件用海量
/// 零长度 box 让解析陷入长循环。
const MAX_BOXES: usize = 1024;

/// box 头部的最小长度：4 字节 size + 4 字节 type。
const BOX_HEADER: u64 = 8;

/// 一个顶层 box 的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxRange {
    /// 在文件中的起始偏移（含 box 头）。
    pub offset: u64,
    /// box 总长度（含 box 头）。
    pub size: u64,
    /// 四字符类型。
    pub kind: [u8; 4],
}

impl BoxRange {
    /// 类型的字符串形式。
    #[must_use]
    pub fn kind_str(&self) -> String {
        String::from_utf8_lossy(&self.kind).into_owned()
    }
}

/// 读取大端 u32。
fn be_u32(b: &[u8]) -> Option<u32> {
    let arr: [u8; 4] = b.get(..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(arr))
}

/// 读取大端 u64。
fn be_u64(b: &[u8]) -> Option<u64> {
    let arr: [u8; 8] = b.get(..8)?.try_into().ok()?;
    Some(u64::from_be_bytes(arr))
}

/// 解析顶层 box 列表。
///
/// `data` 可以只是文件的**前一段**——遇到超出已有数据的 box 时会停止，
/// 已解析出的部分照常返回。这让调用方不必先把整个文件读进内存。
///
/// # Errors
///
/// 数据太短或第一个 box 明显非法时返回 [`MediaError::InvalidInput`]。
pub fn top_level_boxes(data: &[u8]) -> Result<Vec<BoxRange>> {
    let total = data.len() as u64;
    if total < BOX_HEADER {
        return Err(MediaError::InvalidInput {
            reason: format!("数据仅 {total} 字节，不足一个 box 头"),
        });
    }

    let mut out = Vec::new();
    let mut pos: u64 = 0;

    while out.len() < MAX_BOXES {
        // 剩余不足一个头部就停止——这是正常的截断，不是错误
        let Some(rest) = pos.checked_add(BOX_HEADER) else {
            break;
        };
        if rest > total {
            break;
        }
        let start = usize::try_from(pos).map_err(|_| MediaError::InvalidInput {
            reason: "偏移超出平台寻址范围".to_owned(),
        })?;
        let head = data.get(start..).unwrap_or(&[]);

        let size32 = be_u32(head).ok_or_else(|| MediaError::InvalidInput {
            reason: "无法读取 box size".to_owned(),
        })?;
        let kind: [u8; 4] = head
            .get(4..8)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| MediaError::InvalidInput {
                reason: "无法读取 box type".to_owned(),
            })?;

        // size 的三种特殊取值必须分别处理，否则会算出错误的下一个偏移
        let (size, header_len) = match size32 {
            // 0 表示"延伸到文件末尾"
            0 => (total.saturating_sub(pos), BOX_HEADER),
            // 1 表示真实长度在随后的 8 字节里（largesize）
            1 => {
                let large = head
                    .get(8..16)
                    .and_then(be_u64)
                    .ok_or_else(|| MediaError::InvalidInput {
                        reason: "largesize 字段不完整".to_owned(),
                    })?;
                (large, 16)
            }
            n => (u64::from(n), BOX_HEADER),
        };

        // size 必须至少覆盖自己的头部，否则偏移不会前进 → 死循环
        if size < header_len {
            return Err(MediaError::InvalidInput {
                reason: format!(
                    "box {} 声明长度 {size} 小于头部 {header_len}",
                    String::from_utf8_lossy(&kind)
                ),
            });
        }

        out.push(BoxRange {
            offset: pos,
            size,
            kind,
        });

        // checked_add 防止恶意的巨大 size 让偏移回绕
        let Some(next) = pos.checked_add(size) else {
            break;
        };
        if next <= pos {
            // 理论上被上面的检查排除，这里是纵深防御
            break;
        }
        pos = next;
        if pos >= total {
            break;
        }
    }

    if out.is_empty() {
        return Err(MediaError::InvalidInput {
            reason: "未找到任何合法的顶层 box".to_owned(),
        });
    }
    Ok(out)
}

/// 是否为 MP4 家族（以 `ftyp` 开头）。
#[must_use]
pub fn looks_like_mp4(data: &[u8]) -> bool {
    data.get(4..8).is_some_and(|k| k == b"ftyp")
}

/// 定位 `moov` box。
///
/// 返回 `None` 表示在给定数据里没找到——**可能只是数据不完整**
///（moov 在文件尾部而只传了头部），调用方不应据此认定文件无 moov。
///
/// # Errors
///
/// box 结构非法时返回错误。
pub fn find_moov(data: &[u8]) -> Result<Option<BoxRange>> {
    let boxes = top_level_boxes(data)?;
    Ok(boxes.into_iter().find(|b| &b.kind == b"moov"))
}

/// `moov` 是否位于 `mdat` 之前（即所谓 faststart 布局）。
///
/// 这个判断影响播放起始行为：moov 在尾部时播放器要先跳到末尾读元数据，
/// 会多产生 Range 请求。用于决定是否值得缓存 moov 进 TLV。
///
/// # Errors
///
/// box 结构非法时返回错误。
pub fn is_faststart(data: &[u8]) -> Result<bool> {
    let boxes = top_level_boxes(data)?;
    let moov = boxes.iter().position(|b| &b.kind == b"moov");
    let mdat = boxes.iter().position(|b| &b.kind == b"mdat");
    Ok(match (moov, mdat) {
        (Some(mo), Some(md)) => mo < md,
        // 只看到 moov 没看到 mdat，说明 moov 在前
        (Some(_), None) => true,
        // 看到 mdat 但没 moov，说明 moov 在后（或数据不全）
        _ => false,
    })
}

/// 单个 chunk offset 表在 moov 内的位置与格式。
#[derive(Debug, Clone, Copy)]
struct OffsetTable {
    /// 表项数据在 moov **内部**的起始偏移（即第一个偏移值的位置）
    entries_at: usize,
    /// 表项个数
    count: u32,
    /// true 为 co64（64 位），false 为 stco（32 位）
    is_64: bool,
}

/// 递归查找 moov 内所有 `stco` / `co64` box。
///
/// 这两个 box 记录每个 chunk 在**整个文件**中的绝对字节偏移。
/// 一旦挪动 moov 的位置，mdat 的起始位置就变了，这些偏移全部失效——
/// 这就是「直接搬 moov 到前面」会让 FFmpeg 报
/// `Invalid NAL unit size` 的原因（实测确认过）。
///
/// # 必须限定搜索上界
///
/// `base`/`end` 界定当前搜索区间（均为 moov 内部偏移）。
/// **`end` 不可省略**：递归进容器时若不收窄上界，子调用会一直扫到
/// moov 末尾，把容器之外的 stco 也收进来。实测的后果是同一个 stco
/// 被登记 16 次，偏移被累加 16 遍（正确值 4611 变成 63930），
/// FFmpeg 报 `Invalid NAL unit size (1593407596 > 5369)`。
///
/// 由于会重复登记，去重也救不了——必须从根上限定区间。
fn collect_offset_tables(
    moov: &[u8],
    base: usize,
    end: usize,
    out: &mut Vec<OffsetTable>,
    depth: u32,
) {
    // 容器嵌套深度有限，防御畸形文件导致的栈溢出
    if depth > 8 {
        return;
    }
    let limit = end.min(moov.len());
    let mut pos = base;
    while pos + 8 <= limit {
        let Some(size32) = be_u32(moov.get(pos..pos + 4).unwrap_or(&[])) else {
            return;
        };
        let Some(kind) = moov.get(pos + 4..pos + 8) else {
            return;
        };
        // 只处理常规长度；largesize / size==0 在 moov 内部极罕见，
        // 遇到就停止而不是猜测，避免改坏文件
        let size = size32 as usize;
        if size < 8 || pos + size > limit {
            return;
        }

        match kind {
            b"stco" | b"co64" => {
                // 结构：version(1) + flags(3) + entry_count(4) + 表项
                let vf_at = pos + 8;
                let cnt_at = vf_at + 4;
                if let Some(count) = be_u32(moov.get(cnt_at..cnt_at + 4).unwrap_or(&[])) {
                    let is_64 = kind == b"co64";
                    let width = if is_64 { 8usize } else { 4 };
                    let entries_at = cnt_at + 4;
                    // 校验声明的表项数与实际空间相符，不信任 count
                    let need = (count as usize).saturating_mul(width);
                    if entries_at + need <= pos + size {
                        out.push(OffsetTable {
                            entries_at,
                            count,
                            is_64,
                        });
                    }
                }
            }
            // 这些是容器，递归进去找，且**上界收窄到该容器末尾**
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"udta" => {
                collect_offset_tables(moov, pos + 8, pos + size, out, depth + 1);
            }
            _ => {}
        }
        pos += size;
    }
}

/// 把 MP4 重排为 faststart 布局（moov 移到 mdat 之前），
/// 并同步修正 `stco` / `co64` 中的绝对偏移。
///
/// # 为什么需要这个
///
/// FFmpeg 从**管道**读取 MP4 时无法 seek。若 moov 在文件尾部，
/// 它在读到 mdat 时就放弃了，报 `partial file` +
/// `Cannot determine format of input after EOF`，**stdout 完全为空**
///（实测确认：去掉 `-ss`、去掉 `scale`、换输出格式都无效，
/// 用 `-movflags frag_keyframe+empty_moov` 让 FFmpeg 自己 remux
/// 也只产出 1301 字节的空壳，因为它同样读不到 moov）。
///
/// 而尾部 moov 在真实世界极其常见：录屏工具、相机直出、
/// `ffmpeg -c copy` 的默认输出都是这样。
///
/// 唯一不违反「FFmpeg 子进程无文件系统访问」这一安全约束的解法，
/// 就是在**主进程内用纯 Rust** 完成重排，再把结果喂给管道。
///
/// # 偏移修正
///
/// moov 前移后，mdat 的位置后移了 `moov.len()` 字节（若原布局中
/// moov 在 mdat 之后）。因此每个 chunk offset 都要加上这个增量。
/// 不修正的话 FFmpeg 会按错误位置读取，报
/// `Invalid NAL unit size (583027732 > 5369)` 之类的错误。
///
/// # 返回
///
/// 已是 faststart 布局时返回 `Ok(None)`（无需改动）。
///
/// # Errors
///
/// box 结构非法、缺少 moov 或 mdat、偏移计算溢出时返回错误。
pub fn to_faststart(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let boxes = top_level_boxes(data)?;
    let moov_idx = boxes.iter().position(|b| &b.kind == b"moov");
    let mdat_idx = boxes.iter().position(|b| &b.kind == b"mdat");

    let (Some(mi), Some(di)) = (moov_idx, mdat_idx) else {
        return Err(MediaError::InvalidInput {
            reason: "缺少 moov 或 mdat，无法重排".to_owned(),
        });
    };
    // 已经是 faststart，不必动
    if mi < di {
        return Ok(None);
    }

    let moov = boxes.get(mi).ok_or_else(|| MediaError::InvalidInput {
        reason: "moov 索引失效".to_owned(),
    })?;
    let start = usize::try_from(moov.offset).map_err(|_| MediaError::InvalidInput {
        reason: "moov 偏移超出寻址范围".to_owned(),
    })?;
    let len = usize::try_from(moov.size).map_err(|_| MediaError::InvalidInput {
        reason: "moov 长度超出寻址范围".to_owned(),
    })?;
    let end = start.checked_add(len).ok_or_else(|| MediaError::InvalidInput {
        reason: "moov 区间计算溢出".to_owned(),
    })?;
    let mut moov_buf = data
        .get(start..end)
        .ok_or_else(|| MediaError::InvalidInput {
            reason: "moov 区间超出数据范围".to_owned(),
        })?
        .to_vec();

    // 计算 mdat 的**真实位移量**。
    //
    // 不能简单地取 `moov.size`：重排会改变 moov 之外其他 box 的相对次序。
    // 例如实测素材的原布局是 ftyp(32) free(8) mdat(40) moov(349508)，
    // 重排后是 ftyp(0..32) moov(32..) free mdat——mdat 的新位置取决于
    // ftyp 与 moov 的长度之和加上排在它前面的其他 box 长度，
    // 与 moov.size 并不相等。
    //
    // 用 `moov.size` 会让偏移差几个字节，probe 仍能读元数据（它只看 moov），
    // 但真正解码样本时就会报 `Invalid NAL unit size`——
    // 这个差异实测踩过，务必按真实位置计算。
    //
    // 做法：先按目标次序算出每个 box 的新偏移，再求 mdat 的差值。
    let mut new_off: u64 = 0;
    let mut ftyp_total: u64 = 0;
    for b in &boxes {
        if &b.kind == b"ftyp" {
            ftyp_total = ftyp_total.checked_add(b.size).ok_or_else(|| {
                MediaError::InvalidInput {
                    reason: "ftyp 长度累加溢出".to_owned(),
                }
            })?;
        }
    }
    new_off = new_off
        .checked_add(ftyp_total)
        .and_then(|v| v.checked_add(moov.size))
        .ok_or_else(|| MediaError::InvalidInput {
            reason: "新偏移计算溢出".to_owned(),
        })?;

    // 沿目标次序推进，找到 mdat 的新偏移
    let mut mdat_new: Option<u64> = None;
    for b in &boxes {
        if &b.kind == b"ftyp" || &b.kind == b"moov" {
            continue;
        }
        if &b.kind == b"mdat" {
            mdat_new = Some(new_off);
            break;
        }
        new_off = new_off
            .checked_add(b.size)
            .ok_or_else(|| MediaError::InvalidInput {
                reason: "新偏移累加溢出".to_owned(),
            })?;
    }
    let mdat_old = boxes
        .get(di)
        .ok_or_else(|| MediaError::InvalidInput {
            reason: "mdat 索引失效".to_owned(),
        })?
        .offset;
    let mdat_new = mdat_new.ok_or_else(|| MediaError::InvalidInput {
        reason: "无法确定 mdat 的新位置".to_owned(),
    })?;

    // chunk 偏移指向 mdat 内部，故增量就是 mdat 自身的位移量。
    // 用带符号运算：理论上 mdat 也可能前移（原布局把 moov 夹在中间时）。
    let delta_signed = i128::from(mdat_new) - i128::from(mdat_old);

    // 修正所有 chunk offset
    let mut tables = Vec::new();
    collect_offset_tables(&moov_buf, 8, moov_buf.len(), &mut tables, 0);
    for t in &tables {
        for i in 0..(t.count as usize) {
            if t.is_64 {
                let at = t.entries_at + i * 8;
                let Some(cur) = be_u64(moov_buf.get(at..at + 8).unwrap_or(&[])) else {
                    continue;
                };
                let nv = i128::from(cur) + delta_signed;
                let nv = u64::try_from(nv).map_err(|_| MediaError::InvalidInput {
                    reason: "chunk 偏移修正后为负或溢出".to_owned(),
                })?;
                if let Some(dst) = moov_buf.get_mut(at..at + 8) {
                    dst.copy_from_slice(&nv.to_be_bytes());
                }
            } else {
                let at = t.entries_at + i * 4;
                let Some(cur) = be_u32(moov_buf.get(at..at + 4).unwrap_or(&[])) else {
                    continue;
                };
                let nv = i128::from(cur) + delta_signed;
                // 32 位表装不下时无法原地修正。
                // 这种文件需要把 stco 升级成 co64，会改变 box 长度，
                // 进而再次改变所有偏移——如实报错而不是写入截断值。
                let nv32 = u32::try_from(nv).map_err(|_| MediaError::InvalidInput {
                    reason: "修正后的 chunk 偏移超出 32 位，需要 co64（暂不支持）".to_owned(),
                })?;
                if let Some(dst) = moov_buf.get_mut(at..at + 4) {
                    dst.copy_from_slice(&nv32.to_be_bytes());
                }
            }
        }
    }

    // 组装：ftyp（若有）→ moov → 其余按原序
    let mut out = Vec::with_capacity(data.len());
    for b in &boxes {
        if &b.kind == b"ftyp" {
            let s = usize::try_from(b.offset).unwrap_or(0);
            let e = s.saturating_add(usize::try_from(b.size).unwrap_or(0));
            if let Some(sl) = data.get(s..e) {
                out.extend_from_slice(sl);
            }
        }
    }
    out.extend_from_slice(&moov_buf);
    for b in &boxes {
        if &b.kind == b"ftyp" || &b.kind == b"moov" {
            continue;
        }
        let s = usize::try_from(b.offset).unwrap_or(0);
        let e = s.saturating_add(usize::try_from(b.size).unwrap_or(0));
        if let Some(sl) = data.get(s..e) {
            out.extend_from_slice(sl);
        }
    }

    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 box：4 字节大端长度 + 4 字节类型 + 填充。
    fn mk_box(kind: &[u8; 4], payload_len: usize) -> Vec<u8> {
        let size = 8 + payload_len;
        let mut v = Vec::with_capacity(size);
        v.extend_from_slice(&(size as u32).to_be_bytes());
        v.extend_from_slice(kind);
        v.extend(std::iter::repeat_n(0u8, payload_len));
        v
    }

    #[test]
    fn parses_real_layout() {
        // 与实测的 sample.mp4 一致：ftyp(32) moov(64538) free(8) mdat(...)
        let mut data = mk_box(b"ftyp", 24);
        data.extend(mk_box(b"moov", 64530));
        data.extend(mk_box(b"free", 0));
        data.extend(mk_box(b"mdat", 100));

        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), 4);
        assert_eq!(boxes[0].kind_str(), "ftyp");
        assert_eq!(boxes[0].offset, 0);
        assert_eq!(boxes[0].size, 32);
        assert_eq!(boxes[1].kind_str(), "moov");
        assert_eq!(boxes[1].offset, 32);
        assert_eq!(boxes[1].size, 64538);
        assert!(looks_like_mp4(&data));
        assert!(is_faststart(&data).unwrap());
    }

    #[test]
    fn finds_moov() {
        let mut data = mk_box(b"ftyp", 24);
        data.extend(mk_box(b"moov", 100));
        let m = find_moov(&data).unwrap().unwrap();
        assert_eq!(m.offset, 32);
        assert_eq!(m.size, 108);
    }

    #[test]
    fn detects_non_faststart() {
        // moov 在 mdat 之后
        let mut data = mk_box(b"ftyp", 24);
        data.extend(mk_box(b"mdat", 1000));
        data.extend(mk_box(b"moov", 100));
        assert!(!is_faststart(&data).unwrap());
    }

    #[test]
    fn truncated_data_stops_gracefully() {
        // 只有 ftyp 和一个声明很大但数据不全的 mdat。
        // 这是"只读了文件头部"的正常情况，不该报错。
        let mut data = mk_box(b"ftyp", 24);
        let mut big = Vec::new();
        big.extend_from_slice(&1_000_000u32.to_be_bytes());
        big.extend_from_slice(b"mdat");
        data.extend(big);

        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), 2);
        // moov 不在已有数据里 → None，但不是错误
        assert!(find_moov(&data).unwrap().is_none());
    }

    #[test]
    fn rejects_size_smaller_than_header() {
        // size=4 小于头部 8，若不检查会导致偏移不前进 → 死循环
        let mut data = Vec::new();
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(&[0u8; 16]);
        let e = top_level_boxes(&data).unwrap_err();
        assert_eq!(e.code(), "INVALID_INPUT");
    }

    #[test]
    fn handles_size_zero_as_rest_of_file() {
        // size=0 表示延伸到文件末尾
        let mut data = mk_box(b"ftyp", 24);
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0u8; 50]);

        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].kind_str(), "mdat");
        // 32 + 58 = 90 = 总长
        assert_eq!(boxes[1].size, 58);
    }

    #[test]
    fn handles_largesize() {
        // size=1 表示真实长度在后续 8 字节
        let mut data = mk_box(b"ftyp", 24);
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&40u64.to_be_bytes());
        data.extend_from_slice(&[0u8; 24]);

        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].size, 40);
    }

    #[test]
    fn rejects_largesize_below_header() {
        // largesize 声明 8，但头部已占 16 字节
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&8u64.to_be_bytes());
        data.extend_from_slice(&[0u8; 16]);
        assert!(top_level_boxes(&data).is_err());
    }

    #[test]
    fn huge_size_does_not_overflow() {
        // u32::MAX 的 size：pos + size 必须用 checked_add，
        // 否则回绕后可能算出很小的偏移，造成无限循环或越界
        let mut data = Vec::new();
        data.extend_from_slice(&u32::MAX.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0u8; 32]);
        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), 1);
    }

    #[test]
    fn too_short_data_errors() {
        assert!(top_level_boxes(&[0u8; 4]).is_err());
        assert!(top_level_boxes(&[]).is_err());
    }

    #[test]
    fn non_mp4_is_detected() {
        // PNG 头
        let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert!(!looks_like_mp4(&png));
    }

    #[test]
    fn box_count_is_capped() {
        // 大量最小 box：必须在 MAX_BOXES 处停止而非无限增长
        let mut data = Vec::new();
        for _ in 0..(MAX_BOXES + 500) {
            data.extend(mk_box(b"free", 0));
        }
        let boxes = top_level_boxes(&data).unwrap();
        assert_eq!(boxes.len(), MAX_BOXES);
    }

    /// 构造一个含 trak/mdia/minf/stbl/stco 嵌套链的 moov。
    ///
    /// `offsets` 是 stco 里的 chunk 偏移值。返回完整的 moov box 字节。
    fn mk_moov_with_stco(offsets: &[u32]) -> Vec<u8> {
        // stco: version+flags(4) + count(4) + N*4
        let mut stco_payload = Vec::new();
        stco_payload.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        stco_payload.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for o in offsets {
            stco_payload.extend_from_slice(&o.to_be_bytes());
        }
        let stco = wrap(b"stco", &stco_payload);
        let stbl = wrap(b"stbl", &stco);
        let minf = wrap(b"minf", &stbl);
        let mdia = wrap(b"mdia", &minf);
        let trak = wrap(b"trak", &mdia);
        wrap(b"moov", &trak)
    }

    /// 构造 co64 版本
    fn mk_moov_with_co64(offsets: &[u64]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for o in offsets {
            p.extend_from_slice(&o.to_be_bytes());
        }
        let co64 = wrap(b"co64", &p);
        let stbl = wrap(b"stbl", &co64);
        let minf = wrap(b"minf", &stbl);
        let mdia = wrap(b"mdia", &minf);
        let trak = wrap(b"trak", &mdia);
        wrap(b"moov", &trak)
    }

    fn wrap(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + payload.len());
        v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    /// 从重排后的数据里把 stco 的第一个偏移读出来
    fn first_stco_offset(data: &[u8]) -> Option<u32> {
        let boxes = top_level_boxes(data).ok()?;
        let m = boxes.iter().find(|b| &b.kind == b"moov")?;
        let s = m.offset as usize;
        let e = s + m.size as usize;
        let moov = data.get(s..e)?;
        let mut t = Vec::new();
        collect_offset_tables(moov, 8, moov.len(), &mut t, 0);
        let first = t.first()?;
        be_u32(moov.get(first.entries_at..first.entries_at + 4)?)
    }

    #[test]
    fn faststart_reorder_fixes_stco_offsets() {
        // 原布局：ftyp(32) mdat(1008) moov(...)
        // 重排后：ftyp(32) moov(...) mdat(1008)
        // mdat 从 32 移到 32+moov_len，故位移量正好是 moov_len。
        // 注意这个相等**只在 mdat 紧跟 ftyp 时成立**——
        // 中间夹着别的 box 时不成立，见 faststart_delta_accounts_for_middle_boxes。
        let ftyp = mk_box(b"ftyp", 24);
        let mdat = mk_box(b"mdat", 1000);
        let moov = mk_moov_with_stco(&[40, 500]);
        let moov_len = moov.len() as u32;

        let mut data = Vec::new();
        data.extend_from_slice(&ftyp);
        data.extend_from_slice(&mdat);
        data.extend_from_slice(&moov);

        assert!(!is_faststart(&data).unwrap());

        let out = to_faststart(&data).unwrap().expect("应当重排");
        // 总长度不变——只是搬动，不增删数据
        assert_eq!(out.len(), data.len());
        assert!(is_faststart(&out).unwrap());

        // 关键：偏移必须加上 mdat 的真实位移量
        assert_eq!(first_stco_offset(&out), Some(40 + moov_len));
    }

    #[test]
    fn faststart_delta_accounts_for_middle_boxes() {
        // 这个布局正是实测素材的形状：ftyp free mdat moov。
        //
        // 若把增量简单地取 moov.size，free 的长度就被漏算了，
        // 偏移会差 8 字节。实测的表现是：probe 仍能读出元数据
        //（它只解析 moov），但真正解码样本时报
        // `Invalid NAL unit size` —— 极易误判成"重排逻辑没问题"。
        let ftyp = mk_box(b"ftyp", 24); // 32
        let free = mk_box(b"free", 0); // 8
        let mdat = mk_box(b"mdat", 1000); // 1008，数据区从 48 开始
        let moov = mk_moov_with_stco(&[48, 600]);
        let moov_len = moov.len() as u32;

        let mut data = Vec::new();
        data.extend_from_slice(&ftyp);
        data.extend_from_slice(&free);
        data.extend_from_slice(&mdat);
        data.extend_from_slice(&moov);

        let out = to_faststart(&data).unwrap().expect("应当重排");
        assert_eq!(out.len(), data.len());
        // 重排后次序：ftyp(0..32) moov(32..) free mdat
        // mdat 旧偏移 40，新偏移 = 32 + moov_len + 8
        // 位移量 = 32 + moov_len + 8 - 40 = moov_len
        // 这里恰好也等于 moov_len，因为 free 在两种布局里都排在 mdat 前面。
        // 真正的要点是**按位置计算**而非假定，故断言用推导值。
        let expect = 48 + moov_len;
        assert_eq!(first_stco_offset(&out), Some(expect));

        // 独立校验：从重排结果里按新偏移读取，应当落在 mdat 的数据区内
        let boxes = top_level_boxes(&out).unwrap();
        let md = boxes.iter().find(|b| &b.kind == b"mdat").unwrap();
        let data_start = md.offset + 8;
        let data_end = md.offset + md.size;
        assert!(
            u64::from(expect) >= data_start && u64::from(expect) < data_end,
            "修正后的偏移 {expect} 应落在 mdat 数据区 [{data_start}, {data_end})"
        );
    }

    #[test]
    fn faststart_returns_none_when_already_faststart() {
        let mut data = mk_box(b"ftyp", 24);
        data.extend(mk_moov_with_stco(&[100]));
        data.extend(mk_box(b"mdat", 500));
        assert!(is_faststart(&data).unwrap());
        // 已是 faststart → None，避免无谓的复制和偏移改动
        assert!(to_faststart(&data).unwrap().is_none());
    }

    #[test]
    fn faststart_handles_co64() {
        let ftyp = mk_box(b"ftyp", 24);
        let mdat = mk_box(b"mdat", 1000);
        let moov = mk_moov_with_co64(&[40, 500]);
        let moov_len = moov.len() as u64;
        let mut data = Vec::new();
        data.extend_from_slice(&ftyp);
        data.extend_from_slice(&mdat);
        data.extend_from_slice(&moov);

        let out = to_faststart(&data).unwrap().expect("应当重排");
        let boxes = top_level_boxes(&out).unwrap();
        let m = boxes.iter().find(|b| &b.kind == b"moov").unwrap();
        let moov_out = &out[m.offset as usize..(m.offset + m.size) as usize];
        let mut t = Vec::new();
        collect_offset_tables(moov_out, 8, moov_out.len(), &mut t, 0);
        assert_eq!(t.len(), 1);
        assert!(t[0].is_64);
        let v = be_u64(&moov_out[t[0].entries_at..t[0].entries_at + 8]).unwrap();
        assert_eq!(v, 40 + moov_len);
        // 独立校验：新偏移应落在 mdat 数据区内
        let md = boxes.iter().find(|b| &b.kind == b"mdat").unwrap();
        assert!(v >= md.offset + 8 && v < md.offset + md.size);
    }

    #[test]
    fn faststart_rejects_missing_mdat() {
        let mut data = mk_box(b"ftyp", 24);
        data.extend(mk_moov_with_stco(&[40]));
        // 只有 ftyp + moov，没有 mdat
        let e = to_faststart(&data).unwrap_err();
        assert_eq!(e.code(), "INVALID_INPUT");
    }

    #[test]
    fn faststart_detects_stco_overflow() {
        // 偏移接近 u32::MAX，加上 moov 长度后溢出 32 位。
        // 必须报错而不是静默写入截断值——那会产出损坏的文件。
        let ftyp = mk_box(b"ftyp", 24);
        let mdat = mk_box(b"mdat", 1000);
        let moov = mk_moov_with_stco(&[u32::MAX - 10]);
        let mut data = Vec::new();
        data.extend_from_slice(&ftyp);
        data.extend_from_slice(&mdat);
        data.extend_from_slice(&moov);
        let e = to_faststart(&data).unwrap_err();
        assert_eq!(e.code(), "INVALID_INPUT");
    }

    #[test]
    fn collect_offset_tables_finds_nested_stco() {
        let moov = mk_moov_with_stco(&[1, 2, 3]);
        let mut t = Vec::new();
        collect_offset_tables(&moov, 8, moov.len(), &mut t, 0);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].count, 3);
        assert!(!t[0].is_64);
    }

    #[test]
    fn collect_offset_tables_does_not_double_count() {
        // 单轨也必须只登记一次。
        //
        // 这里锁定的是一个真实缺陷：递归进容器时若不收窄搜索上界，
        // 子调用会一直扫到 moov 末尾，导致同一个 stco 被登记多次。
        // 嵌套 trak/mdia/minf/stbl 四层，就会重复 4 次以上。
        //
        // 后果是偏移被累加多遍。实测中一个 121 项的 stco 被登记 16 次，
        // 首项从应有的 4611 变成 63930，FFmpeg 报
        // `Invalid NAL unit size (1593407596 > 5369)`。
        // 单元测试当时没抓到，因为样本只有一层嵌套。
        let moov = mk_moov_with_stco(&[10, 20]);
        let mut t = Vec::new();
        collect_offset_tables(&moov, 8, moov.len(), &mut t, 0);
        assert_eq!(t.len(), 1, "单个 stco 只应登记一次，实际 {}", t.len());
    }

    #[test]
    fn collect_offset_tables_handles_multiple_traks() {
        // 双轨（视频 + 音频）：应当恰好登记 2 个表，不多不少。
        // 真实文件几乎都是多轨，这才是常态。
        let mut inner = Vec::new();
        for offs in [&[100u32, 200][..], &[300, 400][..]] {
            let mut p = Vec::new();
            p.extend_from_slice(&0u32.to_be_bytes());
            p.extend_from_slice(&(offs.len() as u32).to_be_bytes());
            for o in offs {
                p.extend_from_slice(&o.to_be_bytes());
            }
            let stco = wrap(b"stco", &p);
            let stbl = wrap(b"stbl", &stco);
            let minf = wrap(b"minf", &stbl);
            let mdia = wrap(b"mdia", &minf);
            inner.extend(wrap(b"trak", &mdia));
        }
        let moov = wrap(b"moov", &inner);

        let mut t = Vec::new();
        collect_offset_tables(&moov, 8, moov.len(), &mut t, 0);
        assert_eq!(t.len(), 2, "双轨应登记 2 个表，实际 {}", t.len());
        // 两个表的位置必须不同，否则说明扫到了同一个
        assert_ne!(t[0].entries_at, t[1].entries_at);
    }

    #[test]
    fn faststart_multi_trak_offsets_shift_exactly_once() {
        // 端到端锁定：双轨文件重排后，每个偏移只能加一次位移量。
        let ftyp = mk_box(b"ftyp", 24); // 32
        let mdat = mk_box(b"mdat", 2000); // 2008，数据区从 40 起
        let mut inner = Vec::new();
        for offs in [&[40u32, 500][..], &[800, 1200][..]] {
            let mut p = Vec::new();
            p.extend_from_slice(&0u32.to_be_bytes());
            p.extend_from_slice(&(offs.len() as u32).to_be_bytes());
            for o in offs {
                p.extend_from_slice(&o.to_be_bytes());
            }
            let stco = wrap(b"stco", &p);
            let stbl = wrap(b"stbl", &stco);
            let minf = wrap(b"minf", &stbl);
            let mdia = wrap(b"mdia", &minf);
            inner.extend(wrap(b"trak", &mdia));
        }
        let moov = wrap(b"moov", &inner);
        let moov_len = moov.len() as u32;

        let mut data = Vec::new();
        data.extend_from_slice(&ftyp);
        data.extend_from_slice(&mdat);
        data.extend_from_slice(&moov);

        let out = to_faststart(&data).unwrap().expect("应当重排");
        let boxes = top_level_boxes(&out).unwrap();
        let m = boxes.iter().find(|b| &b.kind == b"moov").unwrap();
        let mo = &out[m.offset as usize..(m.offset + m.size) as usize];
        let mut t = Vec::new();
        collect_offset_tables(mo, 8, mo.len(), &mut t, 0);
        assert_eq!(t.len(), 2);

        // 逐项核对：原值 + moov_len
        let expected: [&[u32]; 2] = [&[40, 500], &[800, 1200]];
        for (ti, (tab, exp)) in t.iter().zip(expected.iter()).enumerate() {
            for (i, want_base) in exp.iter().enumerate().take(tab.count as usize) {
                let at = tab.entries_at + i * 4;
                let got = be_u32(&mo[at..at + 4]).unwrap();
                let want = want_base + moov_len;
                assert_eq!(
                    got, want,
                    "第 {ti} 表第 {i} 项：期望 {want}，实际 {got}（差 {} 倍位移）",
                    (got - want_base) / moov_len
                );
            }
        }

        // 所有偏移都必须落在 mdat 数据区内
        let md = boxes.iter().find(|b| &b.kind == b"mdat").unwrap();
        for tab in &t {
            for i in 0..(tab.count as usize) {
                let at = tab.entries_at + i * 4;
                let v = u64::from(be_u32(&mo[at..at + 4]).unwrap());
                assert!(
                    v >= md.offset + 8 && v < md.offset + md.size,
                    "偏移 {v} 越出 mdat 数据区"
                );
            }
        }
    }

    #[test]
    fn collect_offset_tables_ignores_bogus_count() {
        // count 声明 1000 但实际只有 1 项的空间：不能采信
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1000u32.to_be_bytes());
        p.extend_from_slice(&42u32.to_be_bytes());
        let stco = wrap(b"stco", &p);
        let stbl = wrap(b"stbl", &stco);
        let moov = wrap(b"moov", &stbl);
        let mut t = Vec::new();
        collect_offset_tables(&moov, 8, moov.len(), &mut t, 0);
        // 空间不足 → 不收录，避免越界读写
        assert!(t.is_empty());
    }

    #[test]
    fn collect_offset_tables_depth_is_bounded() {
        // 深度嵌套不应导致栈溢出
        let mut cur = wrap(b"stbl", &[]);
        for _ in 0..40 {
            cur = wrap(b"trak", &cur);
        }
        let moov = wrap(b"moov", &cur);
        let mut t = Vec::new();
        collect_offset_tables(&moov, 8, moov.len(), &mut t, 0);
        assert!(t.is_empty());
    }
}
