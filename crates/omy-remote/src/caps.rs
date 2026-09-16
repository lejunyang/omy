//! 能力位图：一个远程位置支持哪些操作。
//!
//! # 这是唯一真相
//!
//! 前端的菜单禁用、后端的命令入口校验都读它。两处各自判断迟早漂移，
//! 症状是「菜单是亮的但点了报错」，或者反过来「明明能做却是灰的」。
//!
//! `RemoteScreen.vue` 的注释已经记过这个教训：约束散落在十几个地方，
//! 漏一处就是用户点了加密然后收到一个没人看得懂的错误。

use serde::{Deserialize, Serialize};

/// 一个远程位置支持哪些操作。
///
/// 字段名会被序列化给前端，**改名等于改协议**：前端读到 `undefined` 时
/// `!caps.write` 为真，只读位置反而会变成全部可写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 列目录与下载。所有位置恒为 `true`——连读都不行就不该出现在列表里。
    pub read: bool,
    /// 上传新文件。
    pub write: bool,
    /// 删除。
    pub delete: bool,
    /// 重命名或移动。
    pub rename: bool,
    /// 新建目录。
    pub create_dir: bool,
    /// 能否原地改写已有文件的字节区间。
    ///
    /// 云端**一律 false**：没有部分写 API，改密码槽这类操作要整文件重传。
    /// 与 `write` 分开是因为两者的失败后果完全不同——`write` 失败只是
    /// 没传上去，而误以为支持原地改写会把一个 4 GiB 的文件传两遍。
    pub random_write: bool,
    /// 是否支持 HTTP Range。
    ///
    /// 为 `false` 则无法流式播放，只能整取——那意味着看一个 14 GB 的
    /// 视频要先等它下完。界面据此决定是给「播放」还是「下载后播放」。
    pub range_read: bool,
}

impl Capabilities {
    /// 本地文件系统：全能力。
    #[must_use]
    pub const fn local() -> Self {
        Self {
            read: true,
            write: true,
            delete: true,
            rename: true,
            create_dir: true,
            random_write: true,
            range_read: true,
        }
    }

    /// 只读远程位置。
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            delete: false,
            rename: false,
            create_dir: false,
            random_write: false,
            range_read: true,
        }
    }

    /// 可写的云端位置。
    ///
    /// 注意 `random_write` 仍为 `false`：能上传整个文件，不等于能改其中
    /// 几个字节。
    #[must_use]
    pub const fn cloud_writable() -> Self {
        Self {
            read: true,
            write: true,
            delete: true,
            rename: true,
            create_dir: true,
            random_write: false,
            range_read: true,
        }
    }

    /// 是否有任何写能力。界面据此决定整组写操作是否出现。
    #[must_use]
    pub const fn any_write(&self) -> bool {
        self.write || self.delete || self.rename || self.create_dir
    }
}

impl Default for Capabilities {
    /// 默认**只读**。
    ///
    /// 这个方向是刻意的：新驱动忘了声明能力时，最坏结果是「少了几个
    /// 按钮」，而不是「以为能写结果每次都失败」，更不是对只读位置发起
    /// 删除请求。安全的默认值应当偏保守。
    fn default() -> Self {
        Self::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认必须是只读。
    ///
    /// 不这样会怎样：新加的驱动忘了声明能力，界面会把删除、重命名全部
    /// 点亮，用户点下去才发现失败——而数据可能已经被改了一半。
    #[test]
    fn default_is_read_only() {
        let c = Capabilities::default();
        assert!(c.read);
        assert!(!c.write);
        assert!(!c.any_write());
    }

    /// 可写云端仍不支持原地改写。
    ///
    /// 不这样会怎样：改密码会走进「整文件重传」而界面毫无提示，
    /// 一个 4 GiB 的视频要传两遍。
    #[test]
    fn cloud_writable_has_no_random_write() {
        let c = Capabilities::cloud_writable();
        assert!(c.write);
        assert!(!c.random_write, "云端没有部分写 API");
    }

    /// 序列化字段名不能变。
    ///
    /// 不这样会怎样：前端读到 undefined，`!caps.write` 为真，
    /// 只读位置反而全部可写——这是最危险的失效方向。
    #[test]
    fn field_names_are_stable() {
        let j = serde_json::to_value(Capabilities::read_only()).expect("应可序列化");
        for k in ["read", "write", "delete", "rename", "create_dir", "random_write", "range_read"] {
            assert!(j.get(k).is_some(), "字段 {k} 不能改名或缺失");
        }
        assert_eq!(j["write"], serde_json::json!(false));
    }

    /// 本地是全能力，用于让本地位置走同一套判断而不必特例。
    #[test]
    fn local_is_fully_capable() {
        let c = Capabilities::local();
        assert!(c.any_write());
        assert!(c.random_write);
        assert!(c.range_read);
    }
}
