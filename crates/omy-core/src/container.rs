//! 目录容器（模式 A）：把整个文件夹打包进单个 `.omy`。
//!
//! 对应设计文档 [05 号 §2](../../docs/research/05-container-and-sharding.md)
//! 与决策 D-05。载荷是若干文件字节的顺序拼接，`TLV_FOLDER_INDEX`
//! 记录每个条目在**明文载荷流**中的偏移与长度。
//!
//! # 为什么偏移是明文流偏移
//!
//! 与分块 AEAD 的寻址正交：给定 `offset..offset+size`，直接套用
//! [`payload`](crate::payload) 的区间读取即可，容器内单文件预览
//! 与单文件模式复用同一套逻辑，只多一层基址。
//!
//! # 路径安全
//!
//! 索引来自**不可信文件**。若原样拼接落盘，`..` 或绝对路径会造成
//! 目录穿越写入。[`ContainerIndex::parse`] 在解析阶段即拒绝这类条目，
//! 而不是留给调用方自觉检查——见 [`validate_component`]。

use crate::error::{Error, Result};
use crate::tlv::types::FOLDER_INDEX;
use std::collections::BTreeMap;

/// 构造索引解析错误。
///
/// 索引是 `TLV_FOLDER_INDEX` 的内容，复用既有的 `MalformedTlv`：
/// 错误码 `MALFORMED_TLV` 与退出码 `Corrupted` 的映射都已就位，
/// 无需为容器新增一套错误分类。
const fn bad(reason: &'static str) -> Error {
    Error::MalformedTlv {
        tlv_type: FOLDER_INDEX,
        reason,
    }
}

/// 索引格式版本。与文件格式版本独立演进。
pub const INDEX_VERSION: u16 = 1;

/// 建议的条目数上限。超过时上层应提示改用树形模式（文档 §2.4）。
pub const RECOMMENDED_MAX_ENTRIES: usize = 50_000;

/// 单个路径组件的字节上限。多数文件系统的限制为 255。
pub const MAX_COMPONENT_LEN: usize = 255;

/// 索引自身的字节上限，防止畸形输入触发大量分配。
pub const MAX_INDEX_SIZE: usize = 64 * 1024 * 1024;

/// 条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// 普通文件，占用载荷区间。
    File,
    /// 目录。空目录必须显式记录，否则还原后会丢失。
    Dir,
    /// 符号链接，`target` 有效。
    Symlink,
}

impl EntryKind {
    const fn as_u8(self) -> u8 {
        match self {
            Self::File => 0,
            Self::Dir => 1,
            Self::Symlink => 2,
        }
    }

    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::File),
            1 => Some(Self::Dir),
            2 => Some(Self::Symlink),
            _ => None,
        }
    }
}

/// 跨平台元数据。不支持的项在还原时**明确报告**而非静默丢弃（决策 N3）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryMeta {
    /// 修改时间，Unix 纳秒。
    pub mtime_ns: Option<i128>,
    /// 创建时间，Unix 纳秒。Linux 上无法设置，还原时跳过并报告。
    pub btime_ns: Option<i128>,
    /// POSIX 权限位。Windows 无对应概念，还原时跳过并报告。
    pub mode: Option<u32>,
    /// 属主 uid / gid。跨机器无意义，默认不还原，仅记录。
    pub uid: Option<u32>,
    /// 见 [`Self::uid`]。
    pub gid: Option<u32>,
    /// 扩展属性。Windows 是 ADS，语义不同，跨平台时报告。
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

impl EntryMeta {
    /// 是否完全为空——为空时可省略编码，节省索引体积。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mtime_ns.is_none()
            && self.btime_ns.is_none()
            && self.mode.is_none()
            && self.uid.is_none()
            && self.gid.is_none()
            && self.xattrs.is_empty()
    }
}

/// 一个容器条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEntry {
    /// 路径组件数组。**不是**拼接好的字符串——数组形式天然规避
    /// 分隔符差异与路径注入（文档 §2.2）。
    pub path: Vec<String>,
    /// 条目类型。
    pub kind: EntryKind,
    /// 在明文载荷流中的起始偏移。仅 `File` 有意义。
    pub offset: u64,
    /// 字节长度。仅 `File` 有意义。
    pub size: u64,
    /// 内容哈希 BLAKE2b-256，用于逐文件完整性校验。
    pub hash: Option<[u8; 32]>,
    /// 符号链接目标。仅 `Symlink` 有意义。
    pub target: Option<String>,
    /// 元数据。
    pub meta: EntryMeta,
}

impl ContainerEntry {
    /// 构造一个文件条目。
    #[must_use]
    pub fn file(path: Vec<String>, offset: u64, size: u64) -> Self {
        Self {
            path,
            kind: EntryKind::File,
            offset,
            size,
            hash: None,
            target: None,
            meta: EntryMeta::default(),
        }
    }

    /// 构造一个目录条目。
    #[must_use]
    pub fn dir(path: Vec<String>) -> Self {
        Self {
            path,
            kind: EntryKind::Dir,
            offset: 0,
            size: 0,
            hash: None,
            target: None,
            meta: EntryMeta::default(),
        }
    }

    /// 构造一个符号链接条目。
    #[must_use]
    pub fn symlink(path: Vec<String>, target: String) -> Self {
        Self {
            path,
            kind: EntryKind::Symlink,
            offset: 0,
            size: 0,
            hash: None,
            target: Some(target),
            meta: EntryMeta::default(),
        }
    }

    /// 用 `/` 连接的显示用路径。**不可**直接用于落盘。
    #[must_use]
    pub fn display_path(&self) -> String {
        self.path.join("/")
    }

    /// 载荷区间。仅文件有效。
    #[must_use]
    pub fn range(&self) -> Option<(u64, u64)> {
        if self.kind == EntryKind::File {
            Some((self.offset, self.size))
        } else {
            None
        }
    }
}

/// 校验单个路径组件是否可安全落盘。
///
/// 拒绝空串、`.`、`..`、含分隔符或 NUL 的组件，以及超长组件。
/// 这些若被放过，解压时会写到目标目录之外。
///
/// # Errors
///
/// 组件不安全时返回 [`bad`]。
pub fn validate_component(c: &str) -> Result<()> {
    if c.is_empty() {
        return Err(bad("路径组件为空"));
    }
    if c == "." || c == ".." {
        return Err(bad("路径组件为 . 或 ..，可能造成目录穿越"));
    }
    if c.len() > MAX_COMPONENT_LEN {
        return Err(bad("路径组件超过 255 字节"));
    }
    if c.contains('/') || c.contains('\\') {
        return Err(bad("路径组件含分隔符"));
    }
    if c.contains('\0') {
        return Err(bad("路径组件含 NUL 字节"));
    }
    // Windows 盘符形式，如 "C:"；放过会写到其它盘。
    //
    // 用字符而非字节下标判断：多字节 UTF-8 下 as_bytes()[1] 可能落在
    // 某个字符的中间，既判不准也没有意义。
    {
        let mut ch = c.chars();
        if let (Some(a), Some(b)) = (ch.next(), ch.next())
            && a.is_ascii_alphabetic()
            && b == ':'
        {
            return Err(bad("路径组件形如盘符"));
        }
    }
    // 任何位置的冒号在 Windows 上都会被当作 NTFS 数据流分隔符
    // （如 "a.txt:hidden"），写入会产生意料之外的隐藏流。
    if c.contains(':') {
        return Err(bad("路径组件含冒号"));
    }
    Ok(())
}

/// 把字符串安全截断到不超过 `max` 字节，且不切断 UTF-8 字符。
///
/// 直接 `&s[..max]` 在多字节字符中间会 **panic**——这不是理论风险：
/// 中文文件名每字符 3 字节，任何不对齐的截断点都会命中。
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    // 回退到字符边界；最坏回退 3 字节。
    // 用 saturating_sub 而非 -=：core 禁止任何可能回绕的算术，
    // 即便此处有 end > 0 守卫也不破例，以免后续改动引入缺陷。
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or("")
}

/// 容器目录索引。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerIndex {
    /// 原始根目录名，仅用于显示。
    pub root: String,
    /// 全部条目。
    pub entries: Vec<ContainerEntry>,
}

impl ContainerIndex {
    /// 新建空索引。
    #[must_use]
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            entries: Vec::new(),
        }
    }

    /// 文件条目总字节数——即容器明文载荷的长度。
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.size)
            .sum()
    }

    /// 文件条目数量。
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .count()
    }

    /// 按显示路径查找条目。
    #[must_use]
    pub fn find(&self, display_path: &str) -> Option<&ContainerEntry> {
        self.entries.iter().find(|e| e.display_path() == display_path)
    }

    /// 编码为 `TLV_FOLDER_INDEX` 的明文内容。
    ///
    /// 自定义紧凑二进制格式而非 JSON：索引可能上到 10 MB 量级，
    /// 且要在移动端解析，避免文本格式的体积与解析开销。
    ///
    /// 布局：
    /// ```text
    /// u16  version
    /// u16  root_len       root 的 UTF-8 字节数
    /// u32  entry_count
    /// ...  root 字节
    /// 每个条目：
    ///   u8   kind
    ///   u8   flags        bit0=有 hash, bit1=有 target, bit2=有 meta
    ///   u16  component_count
    ///   u64  offset
    ///   u64  size
    ///   每个组件： u16 len + 字节
    ///   [hash 32B]
    ///   [u16 target_len + 字节]
    ///   [meta 块]
    /// ```
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // root 超长时截断：它只用于显示，不参与落盘路径。
        // 必须按 UTF-8 边界截断，否则多字节字符会被切断。
        let root_str = truncate_utf8(&self.root, u16::MAX as usize);
        let root = root_str.as_bytes();
        let mut out = Vec::with_capacity(64usize.saturating_add(self.entries.len().saturating_mul(48)));

        out.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        let root_len = u16::try_from(root.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&root_len.to_le_bytes());
        let count = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(root);

        for e in &self.entries {
            let mut flags = 0u8;
            if e.hash.is_some() {
                flags |= 1 << 0;
            }
            if e.target.is_some() {
                flags |= 1 << 1;
            }
            if !e.meta.is_empty() {
                flags |= 1 << 2;
            }

            out.push(e.kind.as_u8());
            out.push(flags);
            let ncomp = u16::try_from(e.path.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&ncomp.to_le_bytes());
            out.extend_from_slice(&e.offset.to_le_bytes());
            out.extend_from_slice(&e.size.to_le_bytes());

            for c in e.path.iter().take(ncomp as usize) {
                // 组件已在 add_* 时校验 ≤255 字节，此处截断只是防御性的
                let cs = truncate_utf8(c, u16::MAX as usize);
                let b = cs.as_bytes();
                let l = u16::try_from(b.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&l.to_le_bytes());
                out.extend_from_slice(b);
            }

            if let Some(h) = &e.hash {
                out.extend_from_slice(h);
            }
            if let Some(t) = &e.target {
                let ts = truncate_utf8(t, u16::MAX as usize);
                let b = ts.as_bytes();
                let l = u16::try_from(b.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&l.to_le_bytes());
                out.extend_from_slice(b);
            }
            if !e.meta.is_empty() {
                encode_meta(&e.meta, &mut out);
            }
        }

        out
    }

    /// 从 `TLV_FOLDER_INDEX` 的明文内容解析。
    ///
    /// 同时完成路径安全校验与载荷区间自洽性检查——**不把这些留给调用方**。
    ///
    /// # Errors
    ///
    /// 输入畸形、路径不安全、区间越界或声明长度与实际不符时返回
    /// [`bad`]。
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() > MAX_INDEX_SIZE {
            return Err(bad("索引超过 64 MiB 上限"));
        }
        let mut p = Cursor::new(data);

        let version = p.u16()?;
        if version != INDEX_VERSION {
            return Err(bad("索引版本不支持"));
        }
        let root_len = p.u16()? as usize;
        let count = p.u32()? as usize;

        // 每个条目至少 20 字节（kind+flags+ncomp+offset+size）。
        // 先据此拒绝明显不可能的 count，避免按声明值预分配大量内存。
        let remaining = data.len().saturating_sub(8).saturating_sub(root_len);
        if count.saturating_mul(20) > remaining {
            return Err(bad("条目数与数据长度不符"));
        }

        let root = String::from_utf8(p.take(root_len)?.to_vec())
            .map_err(|_| bad("root 不是合法 UTF-8"))?;

        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = EntryKind::from_u8(p.u8()?)
                .ok_or(bad("未知的条目类型"))?;
            let flags = p.u8()?;
            let ncomp = p.u16()? as usize;
            let offset = p.u64()?;
            let size = p.u64()?;

            if ncomp == 0 {
                return Err(bad("条目路径为空"));
            }
            let mut path = Vec::with_capacity(ncomp.min(64));
            for _ in 0..ncomp {
                let l = p.u16()? as usize;
                let c = String::from_utf8(p.take(l)?.to_vec())
                    .map_err(|_| bad("路径组件不是合法 UTF-8"))?;
                validate_component(&c)?;
                path.push(c);
            }

            let hash = if flags & (1 << 0) != 0 {
                let h: [u8; 32] = p
                    .take(32)?
                    .try_into()
                    .map_err(|_| bad("hash 长度不符"))?;
                Some(h)
            } else {
                None
            };

            let target = if flags & (1 << 1) != 0 {
                let l = p.u16()? as usize;
                Some(
                    String::from_utf8(p.take(l)?.to_vec())
                        .map_err(|_| bad("链接目标不是合法 UTF-8"))?,
                )
            } else {
                None
            };

            let meta = if flags & (1 << 2) != 0 {
                decode_meta(&mut p)?
            } else {
                EntryMeta::default()
            };

            // 非文件条目不得声明载荷区间，否则可用来伪造重叠区间
            if kind != EntryKind::File && (offset != 0 || size != 0) {
                return Err(bad("非文件条目不应声明载荷区间"));
            }
            if kind == EntryKind::Symlink && target.is_none() {
                return Err(bad("符号链接缺少目标"));
            }
            // offset + size 溢出检查
            if kind == EntryKind::File && offset.checked_add(size).is_none() {
                return Err(bad("载荷区间溢出"));
            }

            entries.push(ContainerEntry {
                path,
                kind,
                offset,
                size,
                hash,
                target,
                meta,
            });
        }

        let idx = Self { root, entries };
        idx.validate_layout()?;
        Ok(idx)
    }

    /// 校验文件条目的载荷区间首尾相接、无重叠、无空洞。
    ///
    /// 容器载荷是顺序拼接，任何重叠都意味着索引被篡改或实现有误。
    ///
    /// # Errors
    ///
    /// 区间不连续或路径重复时返回 [`bad`]。
    pub fn validate_layout(&self) -> Result<()> {
        let mut files: Vec<&ContainerEntry> = self
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .collect();
        files.sort_by_key(|e| e.offset);

        let mut expect = 0u64;
        for f in files {
            if f.offset != expect {
                return Err(bad("文件区间不连续或存在重叠"));
            }
            expect = expect
                .checked_add(f.size)
                .ok_or(bad("载荷区间溢出"))?;
        }

        // 路径去重：同名条目会导致还原时相互覆盖
        let mut seen = std::collections::HashSet::with_capacity(self.entries.len());
        for e in &self.entries {
            if !seen.insert(e.display_path()) {
                return Err(bad("存在重复路径"));
            }
        }
        Ok(())
    }
}

/// 顺序读取游标。所有取值都做边界检查，绝不 panic。
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(bad("索引偏移溢出"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or(bad("索引数据意外结束"))?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        self.take(1)?.first().copied().ok_or(bad("读取 u8 失败"))
    }

    fn u16(&mut self) -> Result<u16> {
        let b: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| bad("读取 u16 失败"))?;
        Ok(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| bad("读取 u32 失败"))?;
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64> {
        let b: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| bad("读取 u64 失败"))?;
        Ok(u64::from_le_bytes(b))
    }
}

/// 元数据块：`u8 flags` + 各字段 + xattr 表。
fn encode_meta(m: &EntryMeta, out: &mut Vec<u8>) {
    let mut flags = 0u8;
    if m.mtime_ns.is_some() {
        flags |= 1 << 0;
    }
    if m.btime_ns.is_some() {
        flags |= 1 << 1;
    }
    if m.mode.is_some() {
        flags |= 1 << 2;
    }
    if m.uid.is_some() || m.gid.is_some() {
        flags |= 1 << 3;
    }
    if !m.xattrs.is_empty() {
        flags |= 1 << 4;
    }
    out.push(flags);

    if let Some(t) = m.mtime_ns {
        out.extend_from_slice(&t.to_le_bytes());
    }
    if let Some(t) = m.btime_ns {
        out.extend_from_slice(&t.to_le_bytes());
    }
    if let Some(v) = m.mode {
        out.extend_from_slice(&v.to_le_bytes());
    }
    if m.uid.is_some() || m.gid.is_some() {
        out.extend_from_slice(&m.uid.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&m.gid.unwrap_or(0).to_le_bytes());
    }
    if !m.xattrs.is_empty() {
        let n = u16::try_from(m.xattrs.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&n.to_le_bytes());
        for (k, v) in m.xattrs.iter().take(n as usize) {
            // key 按 UTF-8 边界截断；value 是裸字节，按字节截断即可
            let ks = truncate_utf8(k, u16::MAX as usize);
            let kb = ks.as_bytes();
            let kl = u16::try_from(kb.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&kl.to_le_bytes());
            out.extend_from_slice(kb);

            let vl = u32::try_from(v.len()).unwrap_or(u32::MAX);
            out.extend_from_slice(&vl.to_le_bytes());
            let vend = (vl as usize).min(v.len());
            out.extend_from_slice(v.get(..vend).unwrap_or(&[]));
        }
    }
}

fn decode_meta(p: &mut Cursor<'_>) -> Result<EntryMeta> {
    let flags = p.u8()?;
    let mut m = EntryMeta::default();

    if flags & (1 << 0) != 0 {
        let b: [u8; 16] = p
            .take(16)?
            .try_into()
            .map_err(|_| bad("mtime 长度不符"))?;
        m.mtime_ns = Some(i128::from_le_bytes(b));
    }
    if flags & (1 << 1) != 0 {
        let b: [u8; 16] = p
            .take(16)?
            .try_into()
            .map_err(|_| bad("btime 长度不符"))?;
        m.btime_ns = Some(i128::from_le_bytes(b));
    }
    if flags & (1 << 2) != 0 {
        m.mode = Some(p.u32()?);
    }
    if flags & (1 << 3) != 0 {
        m.uid = Some(p.u32()?);
        m.gid = Some(p.u32()?);
    }
    if flags & (1 << 4) != 0 {
        let n = p.u16()? as usize;
        // xattr 项至少 6 字节，先做粗略下界检查
        for _ in 0..n {
            let kl = p.u16()? as usize;
            let k = String::from_utf8(p.take(kl)?.to_vec())
                .map_err(|_| bad("xattr 键不是合法 UTF-8"))?;
            let vl = p.u32()? as usize;
            if vl > MAX_INDEX_SIZE {
                return Err(bad("xattr 值过大"));
            }
            let v = p.take(vl)?.to_vec();
            m.xattrs.insert(k, v);
        }
    }
    Ok(m)
}

/// 索引构建器：顺序添加条目，自动累加载荷偏移。
///
/// 手工维护 `offset` 极易出错，且错了要到解压时才暴露。
#[derive(Debug, Default)]
pub struct ContainerBuilder {
    index: ContainerIndex,
    cursor: u64,
}

impl ContainerBuilder {
    /// 新建构建器。
    #[must_use]
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            index: ContainerIndex::new(root),
            cursor: 0,
        }
    }

    /// 添加一个文件，偏移自动取当前游标位置。
    ///
    /// # Errors
    ///
    /// 路径组件不安全，或累计偏移溢出时返回错误。
    pub fn add_file(
        &mut self,
        path: Vec<String>,
        size: u64,
        hash: Option<[u8; 32]>,
        meta: EntryMeta,
    ) -> Result<()> {
        for c in &path {
            validate_component(c)?;
        }
        let offset = self.cursor;
        self.cursor = self
            .cursor
            .checked_add(size)
            .ok_or(bad("容器载荷总长溢出"))?;
        self.index.entries.push(ContainerEntry {
            path,
            kind: EntryKind::File,
            offset,
            size,
            hash,
            target: None,
            meta,
        });
        Ok(())
    }

    /// 添加一个目录。
    ///
    /// # Errors
    ///
    /// 路径组件不安全时返回错误。
    pub fn add_dir(&mut self, path: Vec<String>, meta: EntryMeta) -> Result<()> {
        for c in &path {
            validate_component(c)?;
        }
        let mut e = ContainerEntry::dir(path);
        e.meta = meta;
        self.index.entries.push(e);
        Ok(())
    }

    /// 添加一个符号链接。
    ///
    /// # Errors
    ///
    /// 路径组件不安全时返回错误。
    pub fn add_symlink(
        &mut self,
        path: Vec<String>,
        target: String,
        meta: EntryMeta,
    ) -> Result<()> {
        for c in &path {
            validate_component(c)?;
        }
        let mut e = ContainerEntry::symlink(path, target);
        e.meta = meta;
        self.index.entries.push(e);
        Ok(())
    }

    /// 当前累计的明文载荷长度。
    #[must_use]
    pub const fn payload_len(&self) -> u64 {
        self.cursor
    }

    /// 完成构建。
    ///
    /// # Errors
    ///
    /// 布局校验失败时返回错误。
    pub fn finish(self) -> Result<ContainerIndex> {
        self.index.validate_layout()?;
        Ok(self.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let mut b = ContainerBuilder::new("工作资料");
        b.add_dir(vec!["文档".into()], EntryMeta::default()).unwrap();
        b.add_file(
            vec!["文档".into(), "季度报告.docx".into()],
            2451,
            Some([7u8; 32]),
            EntryMeta {
                mtime_ns: Some(1_756_425_600_000_000_000),
                mode: Some(0o100_644),
                ..EntryMeta::default()
            },
        )
        .unwrap();
        b.add_file(vec!["readme.txt".into()], 12, None, EntryMeta::default())
            .unwrap();
        b.add_symlink(
            vec!["链接.txt".into()],
            "文档/季度报告.docx".into(),
            EntryMeta::default(),
        )
        .unwrap();

        let idx = b.finish().unwrap();
        assert_eq!(idx.file_count(), 2);
        assert_eq!(idx.total_size(), 2463);

        let enc = idx.encode();
        let back = ContainerIndex::parse(&enc).unwrap();
        assert_eq!(idx, back);
        assert_eq!(back.root, "工作资料");
    }

    #[test]
    fn offsets_are_contiguous() {
        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a".into()], 100, None, EntryMeta::default())
            .unwrap();
        b.add_file(vec!["b".into()], 250, None, EntryMeta::default())
            .unwrap();
        b.add_file(vec!["c".into()], 0, None, EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();
        assert_eq!(idx.entries[0].range(), Some((0, 100)));
        assert_eq!(idx.entries[1].range(), Some((100, 250)));
        assert_eq!(idx.entries[2].range(), Some((350, 0)));
        assert_eq!(idx.total_size(), 350);
    }

    #[test]
    fn rejects_path_traversal() {
        for bad in ["..", ".", "", "a/b", "a\\b", "C:", "a\0b"] {
            assert!(
                validate_component(bad).is_err(),
                "组件 {bad:?} 本应被拒绝"
            );
        }
        let mut b = ContainerBuilder::new("r");
        assert!(
            b.add_file(vec!["..".into(), "etc".into()], 1, None, EntryMeta::default())
                .is_err()
        );
    }

    #[test]
    fn rejects_colon_in_any_position() {
        // Windows 上冒号是 NTFS 数据流分隔符：
        // 写 "a.txt:hidden" 会创建隐藏的备用数据流而非普通文件。
        // 早期实现只查第 2 字节，这些都会漏过。
        for bad in [
            "C:",              // 盘符
            "z:",              // 小写盘符
            "a.txt:stream",    // 数据流，冒号在中间
            "中文:流",         // 多字节前缀，冒号不在下标 1
            "报告.docx:evil",  // 真实攻击形态
            ":leading",        // 冒号开头
            "trailing:",       // 冒号结尾
        ] {
            assert!(
                validate_component(bad).is_err(),
                "组件 {bad:?} 含冒号，本应被拒绝"
            );
        }
        // 正常的中文名不受影响
        assert!(validate_component("季度报告.docx").is_ok());
        assert!(validate_component("a.txt").is_ok());
    }

    #[test]
    fn utf8_truncation_never_panics() {
        // 直接 &s[..max] 在多字节字符中间会 panic。
        // 中文每字符 3 字节，任何不对齐的截断点都会命中。
        let s = "中文字符串测试";
        for max in 0..=s.len() + 3 {
            let t = truncate_utf8(s, max);
            assert!(t.len() <= max.min(s.len()), "截断结果超长");
            assert!(s.starts_with(t), "截断结果应是原串前缀");
        }
        // 边界：恰好落在字符中间
        assert_eq!(truncate_utf8("中", 1), "");
        assert_eq!(truncate_utf8("中", 2), "");
        assert_eq!(truncate_utf8("中", 3), "中");
        // ASCII 正常截断
        assert_eq!(truncate_utf8("abcdef", 3), "abc");
        // 不超长时原样返回
        assert_eq!(truncate_utf8("ab", 10), "ab");
    }

    #[test]
    fn oversized_root_encodes_without_panic() {
        // root 超过 u16::MAX 字节且是多字节字符：
        // 早期实现在 65535 处按字节切，会切断字符并 panic。
        let long_root = "工".repeat(30_000); // 90,000 字节
        assert!(long_root.len() > u16::MAX as usize);
        let idx = ContainerIndex::new(long_root);
        let enc = idx.encode(); // 不得 panic
        // 编码结果仍可解析，root 被安全截断
        let back = ContainerIndex::parse(&enc).unwrap();
        assert!(back.root.len() <= u16::MAX as usize);
        assert!(back.root.chars().all(|c| c == '工'), "截断处不应出现替换字符");
    }

    #[test]
    fn oversized_xattr_value_encodes_without_panic() {
        let mut b = ContainerBuilder::new("r");
        b.add_file(
            vec!["f".into()],
            0,
            None,
            EntryMeta {
                xattrs: [("键".repeat(100), vec![0u8; 10])].into_iter().collect(),
                ..EntryMeta::default()
            },
        )
        .unwrap();
        let enc = b.finish().unwrap().encode(); // 不得 panic
        assert!(ContainerIndex::parse(&enc).is_ok());
    }

    #[test]
    fn rejects_traversal_in_parsed_data() {
        // 手工构造含 ".." 的索引，模拟被篡改的文件
        let mut idx = ContainerIndex::new("r");
        idx.entries.push(ContainerEntry::file(vec!["ok".into()], 0, 5));
        let mut enc = idx.encode();
        // 把 "ok" 改成 ".." —— 两者都是 2 字节
        let pos = enc.windows(2).position(|w| w == b"ok").unwrap();
        enc[pos] = b'.';
        enc[pos + 1] = b'.';
        assert!(ContainerIndex::parse(&enc).is_err());
    }

    #[test]
    fn rejects_overlapping_ranges() {
        let mut idx = ContainerIndex::new("r");
        idx.entries.push(ContainerEntry::file(vec!["a".into()], 0, 100));
        idx.entries.push(ContainerEntry::file(vec!["b".into()], 50, 100));
        assert!(idx.validate_layout().is_err());
    }

    #[test]
    fn rejects_gap_between_ranges() {
        let mut idx = ContainerIndex::new("r");
        idx.entries.push(ContainerEntry::file(vec!["a".into()], 0, 100));
        idx.entries.push(ContainerEntry::file(vec!["b".into()], 200, 10));
        assert!(idx.validate_layout().is_err());
    }

    #[test]
    fn rejects_duplicate_paths() {
        let mut idx = ContainerIndex::new("r");
        idx.entries.push(ContainerEntry::file(vec!["a".into()], 0, 10));
        idx.entries.push(ContainerEntry::file(vec!["a".into()], 10, 10));
        assert!(idx.validate_layout().is_err());
    }

    #[test]
    fn rejects_dir_with_payload_range() {
        let mut idx = ContainerIndex::new("r");
        let mut d = ContainerEntry::dir(vec!["d".into()]);
        d.size = 100; // 目录不该占载荷
        idx.entries.push(d);
        let enc = idx.encode();
        assert!(ContainerIndex::parse(&enc).is_err());
    }

    #[test]
    fn truncated_input_never_panics() {
        let mut b = ContainerBuilder::new("root");
        b.add_file(vec!["f".into()], 10, Some([1u8; 32]), EntryMeta {
            mtime_ns: Some(1),
            mode: Some(2),
            uid: Some(3),
            gid: Some(4),
            xattrs: [("k".to_string(), vec![9u8; 4])].into_iter().collect(),
            btime_ns: Some(5),
        })
        .unwrap();
        let enc = b.finish().unwrap().encode();
        // 任意截断都必须返回 Err 而非 panic
        for n in 0..enc.len() {
            let _ = ContainerIndex::parse(&enc[..n]);
        }
    }

    #[test]
    fn absurd_entry_count_rejected_without_huge_alloc() {
        // version=1, root_len=0, count=u32::MAX
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(ContainerIndex::parse(&data).is_err());
    }

    #[test]
    fn meta_roundtrip_full() {
        let meta = EntryMeta {
            mtime_ns: Some(-12_345),
            btime_ns: Some(i128::from(i64::MAX)),
            mode: Some(0o777),
            uid: Some(1000),
            gid: Some(1000),
            xattrs: [
                ("user.comment".to_string(), b"hello".to_vec()),
                ("user.empty".to_string(), Vec::new()),
            ]
            .into_iter()
            .collect(),
        };
        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["f".into()], 1, None, meta.clone()).unwrap();
        let enc = b.finish().unwrap().encode();
        let back = ContainerIndex::parse(&enc).unwrap();
        assert_eq!(back.entries[0].meta, meta);
    }

    #[test]
    fn empty_dir_is_preserved() {
        let mut b = ContainerBuilder::new("r");
        b.add_dir(vec!["空目录".into()], EntryMeta::default()).unwrap();
        let idx = b.finish().unwrap();
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.file_count(), 0);
        let back = ContainerIndex::parse(&idx.encode()).unwrap();
        assert_eq!(back.entries[0].kind, EntryKind::Dir);
        assert_eq!(back.entries[0].display_path(), "空目录");
    }

    #[test]
    fn find_by_path() {
        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["dir".into(), "movie.mp4".into()], 5000, None, EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();
        let e = idx.find("dir/movie.mp4").unwrap();
        assert_eq!(e.range(), Some((0, 5000)));
        assert!(idx.find("nope").is_none());
    }

    #[test]
    fn symlink_without_target_rejected() {
        let mut idx = ContainerIndex::new("r");
        let mut s = ContainerEntry::symlink(vec!["l".into()], "t".into());
        s.target = None; // 制造缺失
        idx.entries.push(s);
        let enc = idx.encode();
        assert!(ContainerIndex::parse(&enc).is_err());
    }

    #[test]
    fn long_component_rejected() {
        let long = "a".repeat(MAX_COMPONENT_LEN + 1);
        assert!(validate_component(&long).is_err());
        let ok = "a".repeat(MAX_COMPONENT_LEN);
        assert!(validate_component(&ok).is_ok());
    }
}
