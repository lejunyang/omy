//! [`RemoteStore`]：远程存储的统一契约。
//!
//! # 为什么是异步 trait，而 `BlockSource` 是同步的
//!
//! `omy_core::source::BlockSource` 刻意做成同步（见该模块文档）：它要被
//! 纯 CPU 的解密逻辑调用，异步化会污染整条调用链，还会给 `omy-core`
//! 强加一个 async 运行时依赖。
//!
//! 而 `RemoteStore` 是网络层，天然异步。两者的桥在 [`crate::source`]：
//! `RemoteSource` 在内部 `block_on`，把异步收敛在这一层，不外溢。

use crate::{Capabilities, Result};

/// 远程位置里的一个条目。
///
/// 只有远程真实存在的元信息，不含任何需要解密才能得到的内容——
/// 真实文件名、缩略图那些要先读文件头再解，由上层完成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 在该位置内的标识。
    ///
    /// WebDAV 是路径，云盘 API 多是整数 ID。上层不解释它的含义，
    /// 只原样回传给驱动。
    pub id: String,
    /// 显示名（未解密的文件名，可能是密文名或伪装名）。
    pub name: String,
    /// 是否为目录。
    pub is_dir: bool,
    /// 字节数；目录为 `None`。
    pub size: Option<u64>,
    /// 服务端给出的修改时间（Unix 秒）。
    ///
    /// 用于判断缓存是否失效。**部分 WebDAV 服务端不提供**，此时为 `None`
    /// ——那种情况下缓存只能退化为短 TTL，不能假装能检测到变化。
    pub mtime: Option<i64>,
    /// 服务端给出的 ETag，用于缓存校验。同样可能缺失。
    pub etag: Option<String>,
}

/// 远程存储契约。
///
/// 实现者只需做字节搬运与目录列举；识别 omy 文件、解密、播放全部由
/// 上层复用 `omy-core` 完成。
#[allow(async_fn_in_trait)]
pub trait RemoteStore: Send + Sync {
    /// 该位置的能力**上界**。
    ///
    /// 必须如实声明。声明了做不到的能力，界面会把按钮点亮，用户点下去
    /// 才失败；漏声明能做到的，则是功能凭空消失。
    ///
    /// # 为什么是「上界」而不是「就是这样」
    ///
    /// 有些位置的能力**随目录而变**：「对话即目录」的模型下，同一个位置里
    /// 收藏夹可写、自己的频道可写、别人的公开频道只读、某些群禁止发文件。
    /// 位置级声明成可写，进只读频道后删除菜单照样是亮的、点了才报错；声明成
    /// 只读，则自己的频道也传不上去——两头都不对。
    ///
    /// 所以这里的语义是「这个位置**最多**能做什么」，而
    /// [`RemoteStore::effective_capabilities`] 给出「在这个目录里**实际**能做
    /// 什么」。**界面与命令层都要读后者**，前者只用于「这个位置整体值不值得
    /// 显示写入口」这类粗判断。
    fn capabilities(&self) -> Capabilities;

    /// 在某个目录下的**有效能力**。
    ///
    /// 默认等于位置级能力——这对目录间能力一致的位置（本地、WebDAV）是正确的，
    /// 所以它们不必实现这个方法。
    ///
    /// 能力随目录而变的驱动必须覆盖它，并且**只能收窄、不能放宽**：放宽意味着
    /// 位置级声明不再是上界，那么任何基于位置级做的粗判断（例如「整个位置只读，
    /// 不必显示上传按钮」）都会漏掉该目录，表现为「某个目录里明明能传，
    /// 入口却不出现」。
    ///
    /// # Errors
    ///
    /// 需要向服务端查询权限而查询失败时返回。此时调用方应当按**位置级能力
    /// 收窄后的保守值**处理，而不是假设可写——「没查到」不等于「有权限」。
    async fn effective_capabilities(&self, dir_id: &str) -> Result<Capabilities> {
        let _ = dir_id;
        Ok(self.capabilities())
    }

    /// 位置的可读描述，用于错误信息与日志。
    fn describe(&self) -> String;

    /// 列出目录内容。`dir_id` 为空表示根。
    ///
    /// # Errors
    ///
    /// 网络失败、认证失败或目录不存在时返回。
    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>>;

    /// 读取文件的 `[offset, offset + len)` 区间。
    ///
    /// 这是流式播放的基础：播放器 seek 到哪就只取哪一段。
    /// 实现必须在服务端忽略 `Range`（返回 200 而非 206）时**自行裁剪**，
    /// 否则上层会把整个文件当成一小段密文去解，得到一堆认证失败。
    ///
    /// # Errors
    ///
    /// 网络失败、越界或文件不存在时返回。
    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// 上传一个文件。
    ///
    /// # Errors
    ///
    /// 位置只读、网络失败或空间不足时返回。
    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> Result<Entry> {
        let _ = (dir_id, name, data);
        Err(crate::Error::Unsupported("write"))
    }

    /// 删除。
    ///
    /// # Errors
    ///
    /// 位置只读或目标不存在时返回。
    async fn delete(&self, id: &str) -> Result<()> {
        let _ = id;
        Err(crate::Error::Unsupported("delete"))
    }

    /// 重命名或移动到新名字。
    ///
    /// # Errors
    ///
    /// 位置只读或目标不存在时返回。
    async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
        let _ = (id, new_name);
        Err(crate::Error::Unsupported("rename"))
    }

    /// 新建目录。
    ///
    /// # Errors
    ///
    /// 位置只读或同名已存在时返回。
    async fn create_dir(&self, parent_id: &str, name: &str) -> Result<Entry> {
        let _ = (parent_id, name);
        Err(crate::Error::Unsupported("create_dir"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReadOnlyStore;

    impl RemoteStore for ReadOnlyStore {
        fn capabilities(&self) -> Capabilities {
            Capabilities::read_only()
        }
        fn describe(&self) -> String {
            String::from("test")
        }
        async fn list(&self, _dir_id: &str) -> Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        async fn read_range(&self, _id: &str, _offset: u64, _len: u64) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    /// 没实现的写操作必须报 `Unsupported`，而不是静默成功。
    ///
    /// 不这样会怎样：默认实现若返回 `Ok(())`，删除会「成功」但文件还在，
    /// 用户以为删掉了——而这类错觉在加密工具里可能导致他去删本地副本。
    #[test]
    fn unimplemented_writes_report_unsupported() {
        let s = ReadOnlyStore;
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("建运行时");
        assert!(matches!(
            rt.block_on(s.delete("x")),
            Err(crate::Error::Unsupported("delete"))
        ));
        assert!(matches!(
            rt.block_on(s.write("", "a", b"1")),
            Err(crate::Error::Unsupported("write"))
        ));
        assert!(matches!(
            rt.block_on(s.rename("x", "y")),
            Err(crate::Error::Unsupported("rename"))
        ));
    }

    /// 只读位置的能力位图要与实际行为一致。
    #[test]
    fn caps_match_behaviour() {
        let s = ReadOnlyStore;
        assert!(!s.capabilities().any_write());
    }

    /// 可重试与不可重试要分开。
    ///
    /// 不这样会怎样：认证失败被当成可重试，界面会重试几次才报错，
    /// 而真正该做的是弹出重新登录。
    #[test]
    fn retryable_classification() {
        assert!(crate::Error::RateLimited.is_retryable());
        assert!(crate::Error::Network(String::from("timeout")).is_retryable());
        assert!(!crate::Error::Unauthorized.is_retryable());
        assert!(!crate::Error::NotFound(String::from("a")).is_retryable());
    }
}
