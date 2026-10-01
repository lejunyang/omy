//! [`RemoteSource`]：把远程存储接到 `omy-core` 的 [`BlockSource`]。
//!
//! # 这一层的价值
//!
//! 实现了 `BlockSource` 之后，**播放、缩略图、预览、部分解密全部零改动
//! 可用**——那正是当初把它抽成 trait 的目的。远程与本地对上层完全等价。
//!
//! # 同步与异步的边界收在这里
//!
//! `BlockSource` 是同步的（`omy-core` 不应背上 async 运行时依赖），
//! 而网络层是异步的。桥接就放在这一层：内部 `block_on`，不外溢。
//!
//! 这也是 `omy_core::source` 模块文档里说的「远程实现在自己内部桥接异步」。

use std::sync::Arc;

use blake2::digest::{Update, VariableOutput};

use omy_core::error::{Error as CoreError, Result as CoreResult};
use omy_core::header::FixedHeader;
use omy_core::source::BlockSource;

use crate::cache::{blocks_for, BlockCache, FileCacheStat, BLOCK_SIZE};
use crate::store::RemoteStore;

/// 远程 `.omy` 文件的密文来源。
pub struct RemoteSource<S: RemoteStore> {
    store: Arc<S>,
    /// 位置标识，用作缓存键的一部分。
    place: String,
    /// 条目 id。
    id: String,
    /// omy 文件头。**普通文件为 `None`**——不伪造一个假头部：
    /// `FixedHeader` 里有 argon2 参数、cipher_id 这些只对加密文件有意义的
    /// 字段，硬填假值等于在类型上声称「这是个 omy 文件」，
    /// 哪天有人顺着它去解密会拿到一组编造的参数，而错误出现在很远的地方。
    header: Option<FixedHeader>,
    /// 原始头部字节。远程位置间复制要原样输出完整文件；只保留解析后的
    /// `FixedHeader` 会丢掉扩展 TLV（文件名、缩略图、容器索引等）。普通文件为空。
    header_bytes: Vec<u8>,
    payload_start: u64,
    payload_len: u64,
    /// 缓存键里的「文件版本」：完整密文头部 + 密文大小的 blake2 哈希（十六进制）。
    ///
    /// 缓存键若只有 `位置 ‖ id ‖ 块号`，同名文件在云端被**覆盖更新**（重新加密
    /// 上传、或别的设备传了同名新文件）后，本地仍会命中上一版的密文块，第 0 块
    /// 就 `ChunkAuthFailed`，而且因为缓存一直命中而**无法自愈**。每次加密都会产生
    /// 新的随机 nonce / 盐，头部字节必然改变，把头部哈希混进键里即可让旧块失效。
    /// 头部本身是密文，参与哈希不会泄露明文信息（文件名也不会进路径）。
    cache_version: String,
    cache: Option<BlockCache>,
    /// 用于在同步上下文里驱动异步请求。
    rt: tokio::runtime::Handle,
}

impl<S: RemoteStore> RemoteSource<S> {
    /// 从已读到的文件头构造。
    ///
    /// 头部由调用方先取（扫描阶段本就要读前 480 B），避免这里再发一次请求。
    ///
    /// # Errors
    ///
    /// 头部解析失败时返回。
    pub fn new(
        store: Arc<S>,
        place: impl Into<String>,
        id: impl Into<String>,
        header_bytes: &[u8],
        total_size: u64,
        cache: Option<BlockCache>,
        rt: tokio::runtime::Handle,
    ) -> CoreResult<Self> {
        let header = omy_core::file::peek_header(header_bytes)?;
        let hlen = u64::from(header.header_len);

        // 文件版本：完整头部 + 密文大小的哈希。头部是密文，可安全参与哈希；
        // 详见字段文档中关于「覆盖更新后旧缓存块必须失效」的说明。
        let mut hasher = blake2::Blake2bVar::new(16)
            .unwrap_or_else(|_| blake2::Blake2bVar::new(16).expect("blake2 16 字节合法"));
        hasher.update(header_bytes);
        hasher.update(b"\0");
        hasher.update(&total_size.to_le_bytes());
        let mut digest = [0u8; 16];
        hasher
            .finalize_variable(&mut digest)
            .expect("16 字节输出长度合法");
        let cache_version = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();

        Ok(Self {
            store,
            place: place.into(),
            id: id.into(),
            payload_start: hlen,
            payload_len: total_size.saturating_sub(hlen),
            header: Some(header),
            header_bytes: header_bytes.to_vec(),
            cache_version,
            cache,
            rt,
        })
    }

    /// 传给 [`BlockCache`] 的逻辑条目键：WebDAV id 再混入文件版本，
    /// 使同名文件覆盖更新后不会复用旧密文块（见 `cache_version` 文档）。
    fn cache_key(&self) -> String {
        // 用控制字符分隔，正常 URL/路径里不会出现，避免 id 与版本号粘连
        format!("{}\u{1}{}", self.id, self.cache_version)
    }

    /// 为一个**未加密的普通文件**构造来源。
    ///
    /// # 远程位置里普通文件是主体内容
    ///
    /// Telegram 位置的全部意义就是「把频道里的文件、图片、视频当远程文件
    /// 用」，它们没有 omy 头部。原先只能构造 omy 来源，于是这些文件在界面上
    /// 既打不开也没有右键菜单。
    ///
    /// # 为什么复用本结构而不是另写一个来源
    ///
    /// 块对齐、缓存键、LRU 逐块淘汰、永久层搬运都在这里，而且都是踩过坑才
    /// 写对的（淘汰时序、豁免本次已取块、缓存键含文件版本）。另写一份等于
    /// 把这些坑重新踩一遍，还会出现「omy 文件修了、普通文件还是老样子」。
    ///
    /// 实现上普通文件就是 `payload_start = 0`、`payload_len = 文件全长`：
    /// `fetch_block` 本就不关心内容是不是密文，只按偏移取字节。
    ///
    /// # 缓存键
    ///
    /// 普通文件没有头部可以哈希，改用 `id ‖ 总长度`。云端同名文件被覆盖后
    /// 长度多半会变，旧块随之失效；长度恰好相同的覆盖会命中旧缓存，
    /// 这是**已知取舍**——为此每次都重下整个文件代价过高，而 omy 文件
    /// （真正在意完整性的那些）本来就有头部哈希兜底。
    #[must_use]
    pub fn new_plain(
        store: Arc<S>,
        place: impl Into<String>,
        id: impl Into<String>,
        total_size: u64,
        cache: Option<BlockCache>,
        rt: tokio::runtime::Handle,
    ) -> Self {
        let id = id.into();
        Self {
            store,
            place: place.into(),
            cache_version: format!("plain{total_size}"),
            id,
            payload_start: 0,
            payload_len: total_size,
            header: None,
            header_bytes: Vec::new(),
            cache,
            rt,
        }
    }

    /// 读取远程对象的原始字节区间，供不解密的远程位置间复制使用。
    ///
    /// 普通文件等同 [`Self::read_plain_range`]；omy 文件的头部从已探测到的原始
    /// 头部字节返回，载荷经 `fetch_block` 进入同一套版本化缓存。这样既不会
    /// 丢掉扩展 TLV，也不会为了复制另写一套缓存键和淘汰规则。
    ///
    /// # Errors
    ///
    /// 区间读取或远程分块失败时返回。
    pub fn read_raw_range(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let total = self.payload_start.saturating_add(self.payload_len);
        if offset >= total {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len).min(total);
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));

        if offset < self.payload_start {
            let head_end = end.min(self.payload_start);
            let from = usize::try_from(offset).unwrap_or(usize::MAX);
            let to = usize::try_from(head_end).unwrap_or(usize::MAX);
            out.extend_from_slice(self.header_bytes.get(from..to).unwrap_or_default());
        }

        if end > self.payload_start {
            let payload_offset = offset.saturating_sub(self.payload_start);
            let payload_end = end.saturating_sub(self.payload_start);
            out.extend_from_slice(&self.read_payload_range(
                payload_offset,
                payload_end.saturating_sub(payload_offset),
            )?);
        }
        Ok(out)
    }

    /// 从载荷区域读取一段原始字节；普通文件的载荷就是整个文件。
    fn read_payload_range(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        if len == 0 || offset >= self.payload_len {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len).min(self.payload_len);
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        let mut used: Vec<u64> = Vec::new();
        for b in blocks_for(offset, end - offset) {
            let data = self.fetch_block(b)?;
            used.push(b);
            if let Some(c) = &self.cache {
                let key = self.cache_key();
                let keep: Vec<_> = used
                    .iter()
                    .map(|n| c.path_of(&self.place, &key, *n))
                    .collect();
                c.evict(&keep);
            }
            let bs = b.saturating_mul(BLOCK_SIZE);
            let from = usize::try_from(offset.saturating_sub(bs).min(data.len() as u64))
                .unwrap_or(usize::MAX);
            let to = usize::try_from(end.saturating_sub(bs).min(data.len() as u64))
                .unwrap_or(usize::MAX);
            out.extend_from_slice(data.get(from..to).unwrap_or_default());
        }
        Ok(out)
    }

    /// 读一个**普通文件**的任意区间（不解密，原样返回）。
    ///
    /// 与 `BlockSource::read_ct` 分开命名是有意的：后者的语义是「读密文」，
    /// 对普通文件调用它会让读代码的人以为那是密文、进而以为下游要解密。
    ///
    /// 走的是同一套按块取 + 缓存 + 逐块淘汰。
    ///
    /// # Errors
    ///
    /// 区间越过文件尾、或网络请求失败时返回。
    pub fn read_plain_range(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len).min(self.payload_len);
        if offset >= self.payload_len {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        let mut used: Vec<u64> = Vec::new();
        for b in blocks_for(offset, end - offset) {
            let data = self.fetch_block(b)?;
            used.push(b);
            // 与 read_ct 同样逐块淘汰并豁免本次已取的块：不豁免的话，
            // 读一个比上限还大的文件会一边下一边把刚下的删掉，永远不前进
            if let Some(c) = &self.cache {
                let key = self.cache_key();
                let keep: Vec<_> = used.iter().map(|n| c.path_of(&self.place, &key, *n)).collect();
                c.evict(&keep);
            }
            let bs = b.saturating_mul(BLOCK_SIZE);
            let from = offset.saturating_sub(bs).min(data.len() as u64);
            let to = end.saturating_sub(bs).min(data.len() as u64);
            let (Ok(f), Ok(t)) = (usize::try_from(from), usize::try_from(to)) else {
                return Err(CoreError::ChunkOutOfRange { index: offset, total: self.payload_len });
            };
            out.extend_from_slice(data.get(f..t).unwrap_or(&[]));
        }
        Ok(out)
    }

    /// 密文载荷按块大小向上取整的总块数。
    #[must_use]
    pub fn total_ct_blocks(&self) -> u64 {
        if self.payload_len == 0 {
            0
        } else {
            (self.payload_len.saturating_add(BLOCK_SIZE - 1)) / BLOCK_SIZE
        }
    }

    /// 该文件在本地密文块缓存里的覆盖情况（不发起任何网络请求）。
    #[must_use]
    pub fn cache_stat(&self) -> FileCacheStat {
        let total = self.total_ct_blocks();
        match &self.cache {
            Some(c) => c.stat_file(&self.place, &self.cache_key(), total),
            None => FileCacheStat {
                cached_blocks: 0,
                total_blocks: total,
                cached_bytes: 0,
                fully_cached: false,
                // 连缓存目录都没有，自然谈不上永久保留
                pinned: false,
            },
        }
    }

    /// 删除该文件的全部本地密文块，返回释放字节数。只读远程位置也允许：
    /// 它只清理本机缓存，绝不向云端发写请求。
    pub fn remove_cached_blocks(&self) -> u64 {
        match &self.cache {
            Some(c) => c.remove_file_blocks(&self.place, &self.cache_key(), self.total_ct_blocks()),
            None => 0,
        }
    }

    /// 把整个文件的密文块都取到本地缓存。
    ///
    /// 「转为永久」之前必须先做这一步：[`RemoteSource::pin`] 只搬运**已经在
    /// 临时层里**的块，没取过的块它搬不了。不预热就 pin 的话，永久层里只会有
    /// 之前碰巧缓存过的那几块，而用户以为整个文件都留下来了——直到离线打开
    /// 失败才发现。那是承诺与事实不符，比不提供这个功能更糟。
    ///
    /// 逐块顺序取而不是并发：并发会同时开多个到同一个服务端的请求，
    /// 在限流敏感的 Telegram 上很容易换来一次 `FLOOD_WAIT`，
    /// 结果是「转为永久」整体变慢而不是变快。
    ///
    /// 已经缓存的块不会重新下载——`fetch_block` 自己会先查缓存。
    ///
    /// # Errors
    ///
    /// 任何一块取失败都立刻返回：半个文件的永久缓存没有意义，
    /// 继续取下去只会让用户等更久才看到同一个失败。
    pub fn prefetch_all(&self) -> CoreResult<u64> {
        self.prefetch_all_with_progress(&mut |_| true)
    }

    /// 同 [`Self::prefetch_all`]，但能报进度、也能被中止。
    ///
    /// `on_progress` 每取完一块调用一次，参数是**累计已取字节**；返回
    /// `false` 表示请求中止，函数随即返回已取到的量。
    ///
    /// # 为什么在分片边界判断中止
    ///
    /// 从外面直接 abort 掉这个任务会在缓存里留下半个文件，而半个文件
    /// 看起来和完整的一模一样——下次读到缺块的位置才会失败，那时已经
    /// 指不到「上次取消过」这件事。让循环自己退出就没有这个问题。
    ///
    /// # Errors
    ///
    /// 取块失败（网络、限流、分片过期）时返回。
    pub fn prefetch_all_with_progress(
        &self,
        on_progress: &mut dyn FnMut(u64) -> bool,
    ) -> CoreResult<u64> {
        let total = self.total_ct_blocks();
        let mut got = 0u64;
        for b in 0..total {
            let data = self.fetch_block(b)?;
            got = got.saturating_add(data.len() as u64);
            if !on_progress(got) {
                break;
            }
        }
        Ok(got)
    }

    /// 把该文件标记为永久保留，并把已缓存的块搬进永久层。
    ///
    /// 这些永久层操作都由 `RemoteSource` 转发而不是让调用方直接用
    /// `BlockCache`：缓存键含文件版本（见 `cache_version`），只有这里能算对。
    /// 让 GUI 自己拼键必然拼错——错了不会报错，只会「标记了却永远不命中」。
    ///
    /// # Errors
    ///
    /// 本机不支持永久缓存、或标记写盘失败时返回。
    pub fn pin(&self) -> crate::Result<u64> {
        match &self.cache {
            Some(c) => c.pin_file(&self.place, &self.cache_key(), self.total_ct_blocks()),
            None => Err(crate::Error::Unsupported("本次会话没有可用的缓存目录")),
        }
    }

    /// 取消永久保留，把块搬回临时层（从此可被淘汰）。
    ///
    /// # Errors
    ///
    /// 标记删除失败时返回。
    pub fn unpin(&self) -> crate::Result<u64> {
        match &self.cache {
            Some(c) => c.unpin_file(&self.place, &self.cache_key(), self.total_ct_blocks()),
            None => Ok(0),
        }
    }

    /// 取消永久保留并直接删掉这些块，返回释放字节数。
    ///
    /// # Errors
    ///
    /// 标记删除失败时返回。
    pub fn unpin_and_drop(&self) -> crate::Result<u64> {
        match &self.cache {
            Some(c) => c.unpin_and_drop(&self.place, &self.cache_key(), self.total_ct_blocks()),
            None => Ok(0),
        }
    }

    /// 该文件是否已被标记为永久保留。
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.cache
            .as_ref()
            .map(|c| c.is_pinned(&self.place, &self.cache_key()))
            .unwrap_or(false)
    }

    /// 按块取密文，优先走缓存。
    ///
    /// 缓存写入点在这里——**拿到密文、尚未解密时**。放到解密之后会把明文
    /// 写进缓存目录，那是威胁模型不允许的。
    fn fetch_block(&self, block: u64) -> CoreResult<Vec<u8>> {
        let key = self.cache_key();
        if let Some(c) = &self.cache
            && let Some(hit) = c.get(&self.place, &key, block) {
                return Ok(hit);
            }

        let abs = self.payload_start.saturating_add(block.saturating_mul(BLOCK_SIZE));
        // 末块可能不足一个 BLOCK_SIZE，要按剩余长度裁剪，
        // 否则会向服务端请求超出文件末尾的区间
        let remain = self
            .payload_len
            .saturating_sub(block.saturating_mul(BLOCK_SIZE));
        let want = remain.min(BLOCK_SIZE);
        if want == 0 {
            return Ok(Vec::new());
        }

        let store = Arc::clone(&self.store);
        let id = self.id.clone();
        let data = self
            .rt
            .block_on(async move { store.read_range(&id, abs, want).await })
            .map_err(|e| CoreError::Io(std::io::Error::other(e.to_string())))?;

        if let Some(c) = &self.cache {
            c.put(&self.place, &key, block, &data);
        }
        Ok(data)
    }
}

/// 普通文件在 `BlockSource::header()` 上的兜底返回值。
///
/// 见 `header()` 的说明：正常分流下取不到它。`plaintext_size = 0`
/// 使得万一取到，表现为「读到空内容」而不是解出一堆乱码。
static EMPTY_HEADER: FixedHeader = FixedHeader {
    version_major: 0,
    version_minor: 0,
    header_len: 0,
    file_uuid: [0u8; 16],
    vault_salt: [0u8; 16],
    flags: 0,
    cipher_id: omy_core::crypto::CipherId::ChaCha20Poly1305,
    kdf_id: 0,
    argon2_m_kib: 0,
    argon2_t: 0,
    argon2_p: 0,
    compress_id: 0,
    slot_count: 0,
    chunk_size: 0,
    plaintext_size: 0,
    base_nonce: [0u8; 7],
    chunk_version: 0,
    tlv_len: 0,
};

impl<S: RemoteStore> BlockSource for RemoteSource<S> {
    fn header(&self) -> &FixedHeader {
        // 普通文件不实现 BlockSource 的解密语义：协议层按
        // `OpenPlaceFile::plain` 分流，普通文件走 read_plain_range，
        // 根本不会调到这里。
        //
        // 真被调到说明分流写错了，那是编程错误而非运行时状况——
        // 但 omy-gui 禁 panic，所以返回一个静态的零值头部让调用方
        // 自己发现不对（plaintext_size = 0 会让它读到空内容），
        // 而不是把整个应用带崩。
        self.header.as_ref().unwrap_or(&EMPTY_HEADER)
    }

    fn read_ct(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset.checked_add(len).ok_or(CoreError::ChunkOutOfRange {
            index: offset,
            total: self.payload_len,
        })?;
        if end > self.payload_len {
            return Err(CoreError::Truncated {
                context: "remote ciphertext range",
                need: usize::try_from(end).unwrap_or(usize::MAX),
                got: usize::try_from(self.payload_len).unwrap_or(usize::MAX),
            });
        }

        // 按块取再拼，而不是直接请求任意区间：块对齐才能让缓存命中，
        // 否则每次 seek 都会留下一块互相重叠、谁也用不上的碎片
        let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        let mut used_blocks: Vec<u64> = Vec::new();
        for b in blocks_for(offset, len) {
            let data = self.fetch_block(b)?;
            used_blocks.push(b);

            // 每取一块就淘汰一次，而不是整个区间读完再淘汰。
            //
            // 时序很重要：一次跨 N 块的读若等到最后才淘汰，缓存会先涨到
            // N 块那么大，上限根本没起作用。逐块淘汰才是真实的流式场景。
            //
            // 豁免本次已取到的块：不豁免的话，读一个比上限还大的文件时
            // 会一边下一边把刚下的块删掉，下一段又得重下——同一块被反复
            // 请求，进度永远不前进。
            if let Some(c) = &self.cache {
                let key = self.cache_key();
                let keep: Vec<_> = used_blocks
                    .iter()
                    .map(|n| c.path_of(&self.place, &key, *n))
                    .collect();
                c.evict(&keep);
            }

            let block_start = b.saturating_mul(BLOCK_SIZE);
            let from = offset.saturating_sub(block_start).min(data.len() as u64);
            let to = end.saturating_sub(block_start).min(data.len() as u64);
            let (Ok(f), Ok(t)) = (usize::try_from(from), usize::try_from(to)) else {
                return Err(CoreError::ChunkOutOfRange { index: offset, total: self.payload_len });
            };
            out.extend_from_slice(data.get(f..t).unwrap_or(&[]));
        }
        Ok(out)
    }

    fn ct_len(&self) -> u64 {
        self.payload_len
    }

    /// 恒为真：UI 据此显示网络状态、调整预读策略。
    fn is_remote(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("{}:{}", self.place, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Entry;
    use crate::{Capabilities, Result as RemoteResult};
    use std::sync::Mutex;

    /// 记录每次请求区间的假存储，用于验证块对齐与缓存命中。
    struct FakeStore {
        data: Vec<u8>,
        calls: Mutex<Vec<(u64, u64)>>,
    }

    impl FakeStore {
        fn new(data: Vec<u8>) -> Self {
            Self { data, calls: Mutex::new(Vec::new()) }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().map(|c| c.len()).unwrap_or(0)
        }
    }

    impl RemoteStore for FakeStore {
        fn capabilities(&self) -> Capabilities {
            Capabilities::read_only()
        }
        fn describe(&self) -> String {
            String::from("fake")
        }
        async fn list(&self, _dir_id: &str) -> RemoteResult<Vec<Entry>> {
            Ok(Vec::new())
        }
        async fn read_range(&self, _id: &str, offset: u64, len: u64) -> RemoteResult<Vec<u8>> {
            if let Ok(mut c) = self.calls.lock() {
                c.push((offset, len));
            }
            let s = usize::try_from(offset).unwrap_or(usize::MAX);
            let e = usize::try_from(offset.saturating_add(len)).unwrap_or(usize::MAX);
            Ok(self.data.get(s..e.min(self.data.len())).unwrap_or(&[]).to_vec())
        }
    }

    /// 造一个真实的加密文件，返回（完整字节, 明文）。
    fn make_file(plain_len: usize) -> (Vec<u8>, Vec<u8>) {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{encrypt, EncryptOptions, RandomMaterial};

        let plain: Vec<u8> = (0..plain_len).map(|i| (i % 251) as u8).collect();
        let salt = [0x33u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"rs", &salt, params).expect("KEK");
        let opts = EncryptOptions {
            filename: Some(String::from("r.bin")),
            chunk_size: omy_core::header::MIN_CHUNK_SIZE,
            argon2: params,
            compress: false,
            ..EncryptOptions::default()
        };
        let enc = encrypt(&plain, &[kek], &salt, &opts, &RandomMaterial::generate())
            .expect("加密");
        (enc.bytes, plain)
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("建运行时")
    }

    /// 远程读出的明文必须与本地加密前完全一致。
    ///
    /// 这是整条链路的端到端验证：块对齐、拼接、解密任何一处错了，
    /// 得到的都不会是原文。
    #[test]
    fn remote_read_matches_plaintext() {
        use omy_core::file::open;
        use omy_core::source::read_source_range;

        let (bytes, plain) = make_file(300_000);
        let total = bytes.len() as u64;
        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();

        let src = RemoteSource::new(
            Arc::clone(&store),
            "test",
            "/a.omy",
            &bytes,
            total,
            None,
            r.handle().clone(),
        )
        .expect("构造来源");

        let kek = {
            use omy_core::crypto::{Argon2Params, Kek};
            Kek::from_password(b"rs", &[0x33u8; 16], Argon2Params::TEST_WEAK).expect("KEK")
        };
        let opened = open(&bytes, &[kek]).expect("打开");

        for (off, len) in [(0u64, 100u64), (65_536, 2000), (299_000, 1000)] {
            let got = read_source_range(&src, &opened, off, len).expect("读取");
            let want = plain
                .get(off as usize..(off + len) as usize)
                .expect("切片");
            assert_eq!(got, want, "区间 ({off},{len}) 不符");
        }
        assert!(src.is_remote(), "远程来源必须自报为远程");
    }

    /// 远程复制读取的是完整原始对象，必须包含扩展头部，且跨头部边界不重不漏。
    ///
    /// 不这样会怎样：只复制密文载荷会生成一个没有文件头的目标文件，上传任务显示
    /// 成功，但目标端无法识别、更无法解密；这是最危险的“成功但产物坏了”。
    #[test]
    fn raw_read_preserves_complete_encrypted_file() {
        let (bytes, _) = make_file(300_000);
        let total = bytes.len() as u64;
        let header_len = u64::from(
            omy_core::file::peek_header(&bytes)
                .expect("解析头部")
                .header_len,
        );
        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();
        let src = RemoteSource::new(
            store,
            "test",
            "/copy.omy",
            &bytes[..usize::try_from(header_len).expect("头部长度")],
            total,
            None,
            r.handle().clone(),
        )
        .expect("构造来源");

        let start = header_len.saturating_sub(32);
        let got = src
            .read_raw_range(start, 128)
            .expect("跨头部边界读取");
        let end = usize::try_from(start + 128).expect("结束偏移");
        assert_eq!(
            got,
            bytes[usize::try_from(start).expect("开始偏移")..end],
            "头部末尾与载荷开头必须连续，不能漏头或重复字节"
        );
    }

    /// 同一块重复读只应请求一次网络。
    ///
    /// 不这样会怎样：缓存形同虚设，播放时每秒几十次请求全部打到服务端，
    /// 很快会被限流。
    #[test]
    fn cache_prevents_repeat_requests() {
        let (bytes, _) = make_file(200_000);
        let total = bytes.len() as u64;
        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();

        let dir = std::env::temp_dir()
            .join(format!("omy_rs_cache_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = BlockCache::new(&dir, 0).expect("建缓存");

        let src = RemoteSource::new(
            Arc::clone(&store),
            "test",
            "/a.omy",
            &bytes,
            total,
            Some(cache),
            r.handle().clone(),
        )
        .expect("构造来源");

        let _ = src.read_ct(0, 1000).expect("首次读");
        let first = store.call_count();
        assert!(first > 0, "首次必须真的发请求");

        let _ = src.read_ct(10, 500).expect("同块再读");
        assert_eq!(store.call_count(), first, "同一块不应再次请求网络");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 同名文件在云端被覆盖更新后，绝不能继续返回上一版的缓存块。
    ///
    /// 不这样会怎样：缓存键只有 `位置 ‖ id ‖ 块号` 时，重新加密上传同名文件
    /// （每次 nonce/盐都不同）后，本地仍命中旧密文块，第 0 块就 ChunkAuthFailed，
    /// 而且因为缓存一直命中、永远不会重新拉取，故障无法自愈。
    #[test]
    fn changed_file_does_not_use_stale_cache() {
        // 同长度明文加密两次：RandomMaterial 随机，头部 nonce 不同，密文必不同
        let (b1, _) = make_file(200_000);
        let (b2, _) = make_file(200_000);
        assert_ne!(b1, b2, "两次加密的 nonce 不同，密文必须不同");

        let store1 = Arc::new(FakeStore::new(b1.clone()));
        let store2 = Arc::new(FakeStore::new(b2.clone()));

        let dir = std::env::temp_dir()
            .join(format!("omy_rs_stale_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = BlockCache::new(&dir, 0).expect("建缓存");
        let r = rt();

        let make_src = |bytes: &Vec<u8>, store: Arc<FakeStore>| {
            RemoteSource::new(
                store,
                "test",
                "/a.omy",
                bytes,
                bytes.len() as u64,
                Some(cache.clone()),
                r.handle().clone(),
            )
            .expect("构造来源")
        };

        // 第一版：读一块，落进缓存
        let src1 = make_src(&b1, store1.clone());
        let _ = src1.read_ct(0, 100).expect("读 v1");

        // 同位置、同 id，但文件已被覆盖成第二版
        let src2 = make_src(&b2, store2.clone());
        let got = src2.read_ct(0, 100).expect("读 v2");

        assert!(
            store2.call_count() > 0,
            "文件更新后必须 miss 旧缓存、真正向新存储发请求"
        );
        let hlen2 = u64::from(
            omy_core::file::peek_header(&b2).expect("解析 v2 头部").header_len,
        ) as usize;
        assert_eq!(
            got,
            b2[hlen2..hlen2 + 100],
            "返回的必须是新版密文，不能是旧缓存块"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 缓存目录里不能出现明文。
    ///
    /// 这是 §8.1 那条硬约束唯一靠得住的验证方式：造一个内容已知的文件，
    /// 走完整读取链路，再扫描整个缓存目录确认那段明文不出现。
    ///
    /// 不这样会怎样：某次重构把缓存写入点挪到解密之后，用户特意加密的
    /// 内容就被明文写回磁盘了，而且没有任何现象能让人察觉。
    #[test]
    fn cache_never_contains_plaintext() {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{encrypt, open, EncryptOptions, RandomMaterial};
        use omy_core::source::read_source_range;

        // 用一段可辨识的明文，且长度足够跨块
        let marker = b"OMY-PLAINTEXT-CANARY-9f3a2b";
        let mut plain = Vec::new();
        while plain.len() < 200_000 {
            plain.extend_from_slice(marker);
        }

        let salt = [0x44u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"canary", &salt, params).expect("KEK");
        let opts = EncryptOptions {
            filename: Some(String::from("c.bin")),
            chunk_size: omy_core::header::MIN_CHUNK_SIZE,
            argon2: params,
            compress: false,
            ..EncryptOptions::default()
        };
        let enc = encrypt(&plain, &[kek], &salt, &opts, &RandomMaterial::generate())
            .expect("加密");
        let bytes = enc.bytes;
        let total = bytes.len() as u64;

        let dir = std::env::temp_dir()
            .join(format!("omy_rs_canary_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = BlockCache::new(&dir, 0).expect("建缓存");

        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();
        let src = RemoteSource::new(
            Arc::clone(&store),
            "test",
            "/c.omy",
            &bytes,
            total,
            Some(cache),
            r.handle().clone(),
        )
        .expect("构造来源");

        let kek2 = Kek::from_password(b"canary", &salt, params).expect("KEK2");
        let opened = open(&bytes, &[kek2]).expect("打开");
        // 走完整解密链路，确保明文确实在内存里出现过
        let got = read_source_range(&src, &opened, 0, 150_000).expect("读取");
        assert!(got.windows(marker.len()).any(|w| w == marker), "先确认明文真的解出来了");

        // 扫描整个缓存目录
        let mut found = false;
        let mut stack = vec![dir.clone()];
        while let Some(p) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&p) else { continue };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(buf) = std::fs::read(&path)
                    && buf.windows(marker.len()).any(|w| w == marker) {
                        found = true;
                    }
            }
        }
        assert!(!found, "缓存目录里出现了明文——缓存写入点必须在解密之前");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 缓存上限小于文件时，顺序读必须能正常完成且不重复下载。
    ///
    /// 不这样会怎样：淘汰时若不豁免本次正在用的块，读一个比上限还大的
    /// 文件会一边下一边把刚下的块删掉——同一块被反复请求，进度永远不前进。
    ///
    /// 这条断言是必需的：另一条 `cache_prevents_repeat_requests` 用的是
    /// 不限容量的缓存，`evict` 会在开头直接返回，根本走不到豁免逻辑，
    /// 因而抓不到这个缺陷。
    #[test]
    fn small_cache_does_not_self_evict() {
        // 要跨多个 BLOCK_SIZE（1 MiB）才能触发淘汰，所以文件必须够大
        let (bytes, _) = make_file(3 * 1024 * 1024 + 1000);
        let total = bytes.len() as u64;
        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();

        let dir = std::env::temp_dir()
            .join(format!("omy_rs_small_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        // 上限只有一块半，远小于整个文件：跨块读时必然触发淘汰
        let cache = BlockCache::new(&dir, BLOCK_SIZE + BLOCK_SIZE / 2).expect("建缓存");
        // 留一份句柄用于事后检查各块是否还在
        let probe = cache.clone();

        let src = RemoteSource::new(
            Arc::clone(&store),
            "test",
            "/big.omy",
            &bytes,
            total,
            Some(cache),
            r.handle().clone(),
        )
        .expect("构造来源");

        // 一次跨多块的读：期间会反复触发 evict
        let len = src.ct_len();
        let got = src.read_ct(0, len).expect("跨块读应当成功");
        assert_eq!(got.len() as u64, len, "必须完整读出，不能因自噬而缺数据");

        let blocks = len.div_ceil(BLOCK_SIZE);
        assert!(blocks >= 3, "这个测试要求文件跨三块以上，否则淘汰逻辑走不到");
        assert_eq!(store.call_count() as u64, blocks, "首轮每块应恰好取一次");

        // 关键断言：本次读用到的块必须全部还在缓存里。
        //
        // 上限只够放一块半，取第 3、4 块时按 LRU 本该把第 1、2 块删掉——
        // 但它们是**本次读正在用的块**，必须豁免。不豁免的话，一次跨块读
        // 会一边下一边把自己先取的块删掉，下次再读同一区间又得重下，
        // 同一块被反复请求、进度永远不前进。
        //
        // 实测过：去掉豁免时块 0、1 的 cached 会变成 false，本断言即失败。
        for b in 0..blocks {
            // 键里含文件版本，需用来源实际使用的缓存键，不能再用裸 id
            let p = probe.path_of("test", &src.cache_key(), b);
            assert!(
                p.exists(),
                "块 {b} 被淘汰了——淘汰时没有豁免本次正在使用的块"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 越界读必须报错，而不是返回短数据。
    ///
    /// 不这样会怎样：上层会把不足的字节当成有效密文去解，
    /// 得到认证失败，而真正的原因是请求越界。
    #[test]
    fn out_of_range_errors() {
        let (bytes, _) = make_file(10_000);
        let total = bytes.len() as u64;
        let store = Arc::new(FakeStore::new(bytes.clone()));
        let r = rt();
        let src = RemoteSource::new(
            store,
            "test",
            "/a.omy",
            &bytes,
            total,
            None,
            r.handle().clone(),
        )
        .expect("构造来源");

        assert!(src.read_ct(0, src.ct_len() + 1).is_err());
        assert!(src.read_ct(u64::MAX, 1).is_err());
        assert!(src.read_ct(0, 0).expect("空读应成功").is_empty());
    }
}
