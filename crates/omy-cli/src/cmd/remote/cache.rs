//! 密文块缓存管理：状态 / 清空 / 永久保留（pin）与取消（unpin）。
//!
//! # 为什么 pin 要经过 RemoteSource
//!
//! 缓存键里含「文件版本」（头部哈希 + 大小），只有 [`RemoteSource`] 算得对。
//! CLI 在这里与 GUI 走同一条构造路径，绝不自己拼键——拼错了不会报错，
//! 只会「标记了却永远不命中」。
//!
//! # 边界
//!
//! `status` / `clear` 是纯本地操作；`pin` 会先把整个文件拉进临时缓存再搬入
//! 永久层（离线可用），因此要读全文件。`unpin` 只取消永久标记，不删已缓存块。

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use omy_remote::cache::BlockCache;
use omy_remote::source::RemoteSource;
use omy_remote::RemoteStore;
use serde_json::json;

use super::{Ctx, abs, cache_root, connect_store, find_place, parent_name, rt};
use crate::output::human_bytes;

/// 缓存子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 显示缓存占用与永久保留清单
    Status,
    /// 清空临时缓存块（永久保留的文件不受影响）
    Clear,
    /// 把一个远程文件整个拉到本地并永久保留（离线可用）
    Pin(PinArgs),
    /// 取消永久保留（块回到临时层，之后可被淘汰）
    Unpin(PinArgs),
}

/// `omy remote cache pin|unpin <位置> <远程文件>`。
#[derive(Debug, Args)]
pub struct PinArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 远程文件路径（WebDAV 路径，如 /backup/a.omy）
    #[arg(value_name = "远程文件")]
    pub path: String,
}

/// 打开当前配置对应的缓存。
fn open_cache(ctx: &Ctx) -> Result<BlockCache> {
    let root = cache_root(ctx.cfg).ok_or_else(|| {
        anyhow!("无法确定缓存目录（既无自定义 remote.cache_dir，也无应用缓存目录）")
    })?;
    BlockCache::new(&root, ctx.cfg.remote.cache_limit)
        .with_context(|| format!("打开缓存目录 {} 失败", root.display()))
}

/// 分发。
pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Status => status(ctx),
        Cmd::Clear => clear(ctx),
        Cmd::Pin(a) => pin(ctx, a, true),
        Cmd::Unpin(a) => pin(ctx, a, false),
    }
}

/// `omy remote cache status`。纯本地。
fn status(ctx: &Ctx) -> Result<()> {
    let cache = open_cache(ctx)?;
    let u = cache.usage();
    let pinned = cache.list_pinned();
    let rows: Vec<_> = pinned
        .iter()
        .map(|p| {
            json!({ "place": p.place, "key": p.key, "blocks": p.total_blocks, "bytes": p.used_bytes })
        })
        .collect();
    let limit_str = if u.temp_limit == 0 {
        String::from("不限")
    } else {
        human_bytes(u.temp_limit)
    };
    let pct = if u.temp_limit == 0 {
        String::new()
    } else {
        format!("（{}）", percent(u.temp_used, u.temp_limit))
    };
    let pin_support = if u.pinning_available {
        ""
    } else {
        "（本机不支持永久缓存）"
    };
    let human = format!(
        "临时缓存: {} / {} {}\n永久保留: {}（{} 个文件）{}\n缓存目录: {}",
        human_bytes(u.temp_used),
        limit_str,
        pct,
        human_bytes(u.pinned_used),
        u.pinned_files,
        pin_support,
        cache.root().display(),
    );
    ctx.out.result(
        &human,
        &json!({
            "temp_used": u.temp_used,
            "temp_limit": u.temp_limit,
            "pinned_used": u.pinned_used,
            "pinned_files": u.pinned_files,
            "pinning_available": u.pinning_available,
            "root": cache.root(),
            "pinned": rows,
        }),
    );
    Ok(())
}

fn percent(used: u64, limit: u64) -> u64 {
    used.checked_mul(100).and_then(|u| u.checked_div(limit)).unwrap_or(0)
}

/// `omy remote cache clear`。纯本地。
fn clear(ctx: &Ctx) -> Result<()> {
    let cache = open_cache(ctx)?;
    cache.clear().context("清空缓存失败")?;
    let u = cache.usage();
    ctx.out.result(
        &format!("已清空临时缓存，当前占用 {}（永久保留未动）", human_bytes(u.temp_used)),
        &json!({ "temp_used": u.temp_used, "pinned_used": u.pinned_used }),
    );
    Ok(())
}

/// `pin=true` 做 pin（先全量拉取再永久保留），否则做 unpin。
fn pin(ctx: &Ctx, a: &PinArgs, do_pin: bool) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    let cache = open_cache(ctx)?;
    let rt = rt()?;
    let store = Arc::new(
        rt.block_on(connect_store(&sp, None))
            .map_err(|e| anyhow!("连接位置 {} 失败: {e}", sp.id))?,
    );

    let id = abs(&a.path);
    // 列出父目录拿到文件大小（远端不发 HEAD，list 顺带给）
    let (parent, name) = parent_name(&id);
    let entries = rt
        .block_on(store.list(&parent))
        .map_err(|e| anyhow!("列目录失败: {e}"))?;
    let ent = entries
        .iter()
        .find(|e| e.id == id || (!e.is_dir && e.name == name))
        .ok_or_else(|| anyhow!("远程文件 {} 不存在", id))?;
    let total = ent.size.unwrap_or(0);
    if total == 0 {
        bail!("远程文件 {} 大小未知或为空，无法缓存", id);
    }

    // 读足够多的头部判断是不是 omy 加密文件。peek_header 要看到完整头部
    // （固定头 + TLV），只读 48 字节可能解析失败，多读一截是一次 Range 请求的事。
    let head_len = total.min(8192) as usize;
    let head = rt
        .block_on(store.read_range(&id, 0, head_len as u64))
        .map_err(|e| anyhow!("读取文件头失败: {e}"))?;

    let source = if omy_core::is_omy_file(&head) {
        RemoteSource::new(
            Arc::clone(&store),
            &sp.id,
            &id,
            &head,
            total,
            Some(cache.clone()),
            rt.handle().clone(),
        )
        .map_err(|e| anyhow!("解析远程文件头失败: {e}"))?
    } else {
        RemoteSource::new_plain(
            Arc::clone(&store),
            &sp.id,
            &id,
            total,
            Some(cache.clone()),
            rt.handle().clone(),
        )
    };

    if do_pin {
        // 必须先把块取到本地：pin 只搬运已在临时层的块，不预热的话永久层里
        // 只有碰巧缓存过的几块——用户以为整份离线可用，点开才发现缺块。
        source.prefetch_all().map_err(|e| anyhow!("预取文件到缓存失败: {e}"))?;
        let bytes = source.pin().map_err(|e| anyhow!("标记永久保留失败: {e}"))?;
        ctx.out.result(
            &format!("已永久保留 {}（{}）", id, human_bytes(bytes)),
            &json!({ "pinned": id, "bytes": bytes }),
        );
    } else {
        let bytes = source.unpin().map_err(|e| anyhow!("取消永久保留失败: {e}"))?;
        ctx.out.result(
            &format!("已取消永久保留 {}（释放 {} 永久层）", id, human_bytes(bytes)),
            &json!({ "unpinned": id, "bytes": bytes }),
        );
    }
    Ok(())
}
