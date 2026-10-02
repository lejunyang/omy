//! 远程文件操作：浏览、上传、下载、跨位置复制。

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use omy_remote::RemoteStore;
use serde_json::json;

use super::{Ctx, abs, connect_store, find_place, parent_name, rt};
use super::stores::AnyStore;
use crate::output::human_bytes;

/// 一次读取的分块大小。下载/跨位置复制按这个粒度边读边写，
/// 而不是把整文件读进内存——虽然本切片复制仍会聚合到目标后一次写出，
/// 但下载到本地是流式的。
const CHUNK: u64 = 1 << 20;

/// `omy remote ls <位置> [路径]`。
#[derive(Debug, Args)]
pub struct LsArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 远程目录路径，默认根目录
    pub path: Option<String>,
}

/// `omy remote upload <位置> <本地文件> <远程目录>`。
#[derive(Debug, Args)]
pub struct UploadArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 本地文件路径
    #[arg(value_name = "本地文件")]
    pub local: PathBuf,
    /// 远程目标目录（WebDAV 路径，如 / 或 /backup）
    #[arg(value_name = "远程目录")]
    pub remote_dir: String,
    /// 允许覆盖已存在的远程同名文件
    #[arg(long)]
    pub force: bool,
}

/// `omy remote download <位置> <远程文件> <本地路径>`。
#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 远程文件路径（WebDAV 路径，如 /backup/a.omy）
    #[arg(value_name = "远程文件")]
    pub remote: String,
    /// 本地保存路径
    #[arg(value_name = "本地路径")]
    pub local: PathBuf,
    /// 允许覆盖已存在的本地文件
    #[arg(long)]
    pub force: bool,
}

/// `omy remote copy <源位置>:<源文件> <目标位置>:<目标路径>`。
#[derive(Debug, Args)]
pub struct CopyArgs {
    /// 源位置与文件，形如 p1:/a.omy
    #[arg(value_name = "源位置:文件")]
    pub source: String,
    /// 目标位置与文件，形如 p2:/b.omy
    #[arg(value_name = "目标位置:路径")]
    pub dest: String,
    /// 允许覆盖已存在的目标远程文件
    #[arg(long)]
    pub force: bool,
}

/// 远程路径里禁止出现 `..` 段。
///
/// WebDAV 服务端未必做路径规范化，放任用户传 `/backup/../其它` 可能越级写到
/// 预料之外的目录；这类「路径穿越」必须在客户端这一层直接拒绝。
fn ensure_safe_remote(path: &str) -> Result<()> {
    if path.split('/').any(|seg| seg == "..") {
        bail!("远程路径不得包含 '..'：{path}");
    }
    Ok(())
}

/// `omy remote ls`。
pub fn ls(ctx: &Ctx, a: &LsArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    let dir = a.path.as_deref().map(abs).unwrap_or_default();
    ensure_safe_remote(&dir)?;

    let rt = rt()?;
    let store = rt
        .block_on(connect_store(&sp))
        .map_err(|e| anyhow!("连接位置 {} 失败: {e}", sp.id))?;
    let entries = rt
        .block_on(store.list(&dir))
        .map_err(|e| anyhow!("列目录失败: {e}"))?;

    let rows: Vec<_> = entries
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "id": e.id,
                "is_dir": e.is_dir,
                "size": e.size,
                "mtime": e.mtime,
            })
        })
        .collect();

    let human = entries
        .iter()
        .map(|e| {
            let kind = if e.is_dir { "d" } else { "-" };
            let size = e.size.map(human_bytes).unwrap_or_default();
            format!("{kind} {:>10}  {}", size, e.name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.out.result(&human, &json!({ "dir": if dir.is_empty() { "/" } else { &dir }, "entries": rows }));
    Ok(())
}

/// `omy remote upload`。
pub fn upload(ctx: &Ctx, a: &UploadArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;

    let meta = std::fs::metadata(&a.local)
        .with_context(|| format!("读取本地文件 {} 失败", a.local.display()))?;
    let size = meta.len();
    let name = a
        .local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("upload.bin"));
    let dir = abs(&a.remote_dir);
    ensure_safe_remote(&dir)?;
    let dir = if dir == "/" { String::new() } else { dir };

    let rt = rt()?;
    let store = rt
        .block_on(connect_store(&sp))
        .map_err(|e| anyhow!("连接位置 {} 失败: {e}", sp.id))?;
    if !store.capabilities().any_write() {
        bail!("位置 {} 是只读的，不能上传", sp.id);
    }

    // 覆盖守卫：同名文件已存在时必须显式 --force，避免静默覆盖已有远程文件。
    if !a.force {
        let exists = rt
            .block_on(store.list(&dir))?
            .iter()
            .any(|e: &omy_remote::Entry| e.name == name);
        if exists {
            bail!("远程已存在同名文件 {name:?}；确认覆盖请加 --force");
        }
    }

    let reader = rt.block_on(async {
        let f = tokio::fs::File::open(&a.local).await?;
        Ok::<_, std::io::Error>(Box::new(f) as Box<dyn tokio::io::AsyncRead + Unpin + Send>)
    }).with_context(|| format!("打开本地文件 {} 失败", a.local.display()))?;

    let entry = rt
        .block_on(store.write_stream(&dir, &name, size, reader))
        .map_err(|e| anyhow!("上传失败: {e}"))?;

    ctx.out.result(
        &format!("已上传 {} → {}{}", a.local.display(), entry.id, if entry.is_dir { "（目录?）" } else { "" }),
        &json!({ "remote": entry.id, "size": size }),
    );
    Ok(())
}

/// 按远程路径解析出条目（拿大小）。
///
/// 必须先知道总大小：WebDAV 的 `read_range` 在 offset 越过文件尾时返回
/// `416 Range Not Satisfiable` 错误，而不是空响应。若靠「读到空就停」循环，
/// 最后一次必然发越界 Range 而崩——这正是端到端测试抓到的真缺陷。
async fn entry_by_path(store: &AnyStore, path: &str) -> Result<omy_remote::Entry> {
    let (parent, name) = parent_name(path);
    let entries = store
        .list(&parent)
        .await
        .map_err(|e| anyhow!("列目录 {parent:?} 失败: {e}"))?;
    entries
        .into_iter()
        .find(|e| e.id == path || (!e.is_dir && e.name == name))
        .ok_or_else(|| anyhow!("远程文件 {path} 不存在"))
}

/// `omy remote download`：分块流式写本地。
pub fn download(ctx: &Ctx, a: &DownloadArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    let id = abs(&a.remote);
    ensure_safe_remote(&id)?;

    // 覆盖守卫：本地文件已存在时必须显式 --force，避免静默覆盖本地文件。
    if a.local.exists() && !a.force {
        bail!("本地已存在 {}；确认覆盖请加 --force", a.local.display());
    }

    let rt = rt()?;
    let store = rt
        .block_on(connect_store(&sp))
        .map_err(|e| anyhow!("连接位置 {} 失败: {e}", sp.id))?;
    let total = rt
        .block_on(entry_by_path(&store, &id))?
        .size
        .ok_or_else(|| anyhow!("远程文件 {id} 大小未知"))?;
    if total == 0 {
        bail!("远程文件 {id} 为空");
    }

    let mut out = std::fs::File::create(&a.local)
        .with_context(|| format!("创建本地文件 {} 失败", a.local.display()))?;
    let mut written = 0u64;
    while written < total {
        let want = CHUNK.min(total - written);
        let buf = rt
            .block_on(store.read_range(&id, written, want))
            .map_err(|e| anyhow!("读取远程文件失败: {e}"))?;
        if buf.is_empty() {
            break;
        }
        std::io::Write::write_all(&mut out, &buf)
            .with_context(|| format!("写入本地文件 {} 失败", a.local.display()))?;
        written = written.saturating_add(buf.len() as u64);
    }
    if written != total {
        bail!("远程文件 {id} 下载不完整：{written}/{total} 字节");
    }
    ctx.out.result(
        &format!("已下载 {} → {}（{}）", id, a.local.display(), human_bytes(written)),
        &json!({ "remote": id, "local": a.local, "bytes": written }),
    );
    Ok(())
}

/// 解析 `位置:路径` 形式的参数。
fn split_place_path(s: &str, what: &str) -> Result<(String, String)> {
    let (place, path) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("{what} 必须是「位置:远程路径」形式，如 p1:/a.omy（收到 {s:?}）"))?;
    Ok((place.to_string(), path.to_string()))
}

/// `omy remote copy`：跨位置复制。
///
/// 两个位置都是 WebDAV，「原样字节搬运」不涉及解密，复制的就是密文
/// （或普通文件字节），两端无需共享密码。
///
/// 传输是**有界流式**的：用一个带背压的管道（`tokio::io::duplex`）把源端分块
/// `read_range` 喂给目标端 `write_stream`，内存里同时只有一两块，不再把整文件
/// 聚合进 Vec——大文件不会 OOM。
pub fn copy(ctx: &Ctx, a: &CopyArgs) -> Result<()> {
    let (src_place, src_path) = split_place_path(&a.source, "源")?;
    let (dst_place, dst_path) = split_place_path(&a.dest, "目标")?;

    let src_sp = find_place(ctx.cfg, &src_place)?;
    let dst_sp = find_place(ctx.cfg, &dst_place)?;

    let rt = rt()?;
    let src = rt
        .block_on(connect_store(&src_sp))
        .map_err(|e| anyhow!("连接源位置 {} 失败: {e}", src_sp.id))?;
    let dst = rt
        .block_on(connect_store(&dst_sp))
        .map_err(|e| anyhow!("连接目标位置 {} 失败: {e}", dst_sp.id))?;
    if !dst.capabilities().any_write() {
        bail!("目标位置 {} 是只读的，不能复制过去", dst_sp.id);
    }

    let src_id = abs(&src_path);
    ensure_safe_remote(&src_id)?;
    let (dst_dir, dst_name) = parent_name(&dst_path);
    ensure_safe_remote(&dst_path)?;

    // 按已知大小分块，避免读越界 Range 触发 416。
    let total = rt
        .block_on(entry_by_path(&src, &src_id))?
        .size
        .ok_or_else(|| anyhow!("源文件 {src_id} 大小未知"))?;
    if total == 0 {
        bail!("源文件 {src_id} 为空");
    }

    // 覆盖守卫：目标已存在同名文件时必须显式 --force。
    if !a.force {
        let exists = rt
            .block_on(dst.list(&dst_dir))?
            .iter()
            .any(|e: &omy_remote::Entry| !e.is_dir && e.name == dst_name);
        if exists {
            bail!("目标已存在 {dst_name:?}；确认覆盖请加 --force");
        }
    }

    // 有界流式：管道缓冲 64 KiB，上传消费得慢时读端会自然背压。
    // dst 与目标路径 move 进 spawn 的上传任务（spawn 要求 'static），
    // src/src_id 留在外层块做读循环；src_id 留一份给结束后的结果打印。
    let src_id_for_report = src_id.clone();
    // dst_dir/dst_name 要 move 进 spawn 的上传任务（'static），先转 owned。
    let dst_dir_owned = dst_dir.clone();
    let dst_name_owned = dst_name.clone();
    let entry = rt.block_on(async move {
        let (pipe_read, mut pipe_write) = tokio::io::duplex(64 * 1024);
        let upload = tokio::spawn(async move {
            dst.write_stream(&dst_dir_owned, &dst_name_owned, total, Box::new(pipe_read)).await
        });

        let mut off = 0u64;
        loop {
            let want = CHUNK.min(total - off);
            let buf = src.read_range(&src_id, off, want).await?;
            if buf.is_empty() {
                break;
            }
            off = off.saturating_add(buf.len() as u64);
            tokio::io::AsyncWriteExt::write_all(&mut pipe_write, &buf).await?;
        }
        drop(pipe_write); // 关闭写端 = 通知上传 EOF

        if off != total {
            bail!("源文件 {src_id} 读取不完整：{off}/{total} 字节");
        }
        upload
            .await
            .map_err(|e| anyhow!("上传任务异常: {e}"))?
            .map_err(|e| anyhow!("写入目标失败: {e}"))
    })?;

    ctx.out.result(
        &format!("已复制 {} → {}（{}）", src_id_for_report, entry.id, human_bytes(total)),
        &json!({ "from": src_id_for_report, "to": entry.id, "bytes": total }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 远程路径必须统一成绝对路径，且父子拆分正确。
    ///
    /// 不这样会怎样：用户写相对路径时拼出 /foo//bar 或漏了前导 /，
    /// WebDAV 服务端要么 404 要么落到意外目录，而命令「成功」。
    #[test]
    fn path_normalization_and_split() {
        assert_eq!(abs("foo/bar.txt"), "/foo/bar.txt");
        assert_eq!(abs("/foo/bar.txt"), "/foo/bar.txt");
        assert_eq!(parent_name("/a/b/c.txt"), ("/a/b".to_string(), "c.txt".to_string()));
        assert_eq!(parent_name("/top.txt"), (String::new(), "top.txt".to_string()));
        assert_eq!(parent_name("rel.txt"), (String::new(), "rel.txt".to_string()));
    }

    /// `位置:路径` 必须有冒号，否则报错。
    #[test]
    fn split_requires_colon() {
        assert!(split_place_path("p1:/a.omy", "源").is_ok());
        assert!(split_place_path("no-colon", "源").is_err());
    }

    /// 远程路径里的 `..` 段必须被拒，正常路径放行。
    ///
    /// 不这样会怎样：`/backup/../etc` 这类路径若被服务端规范化，就越级写到了
    /// 预料之外的目录，而命令照常「成功」。客户端必须在发请求前挡住。
    #[test]
    fn dotdot_path_is_rejected() {
        assert!(ensure_safe_remote("/backup/a.omy").is_ok());
        assert!(ensure_safe_remote("/a/b/c.txt").is_ok());
        assert!(ensure_safe_remote("/backup/../etc/passwd").is_err());
        assert!(ensure_safe_remote("/../etc").is_err());
        assert!(ensure_safe_remote("/a/../b").is_err());
    }
}
