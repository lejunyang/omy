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
///
/// **加字段同样是改协议。** 本结构派生了 `Deserialize`，已落盘的配置或前端传回
/// 的旧 JSON 缺了新字段会让整条反序列化失败。所以新增字段一律带
/// `#[serde(default)]`，且默认值取保守的那一侧——漏声明的后果应当是
/// 「少一个功能」，不能是「承诺了一件做不到的事」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 列目录与下载。所有位置恒为 `true`——连读都不行就不该出现在列表里。
    pub read: bool,
    /// 上传新文件。
    pub write: bool,
    /// 删除。
    pub delete: bool,
    /// 重命名或移动。
    ///
    /// **不是所有位置都有这个概念。** 「对话即目录」的位置上（Telegram），一条
    /// 消息里的文件既没有可改的路径、也没有可移动的去处；改名只能靠「删掉重发」，
    /// 而那是另一件事——换了消息号、丢掉原消息的时间与上下文。这种位置要如实
    /// 声明 `false`，不能硬套一个通用的「可写云端」模板。
    pub rename: bool,
    /// 新建目录。
    ///
    /// 同 `rename`：目录由位置自身的结构决定时（对话列表就是目录列表），
    /// 应用无法「新建」一个目录，声明 `false` 才是实话。
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
    /// 该位置是否支持把搜索**下推到服务端**。
    ///
    /// 为 `false` 时搜索只对**已加载到本地的条目**生效——这是本地目录与 WebDAV
    /// 的现状（前端对已加载列表做子串匹配）。为 `true` 时界面可以把搜索框的语义
    /// 从「过滤当前这一屏」改成「搜索整个位置」，并允许在还没列完的目录里直接搜。
    ///
    /// 两者的差别用户是能感知的：前者搜不到还没滚动到的东西。不区分的话，在一个
    /// 有上万条消息的对话里，搜索框会显得像坏了。
    ///
    /// 默认 `false`，理由同上：少一个功能可以接受，承诺一个做不到的
    /// 「搜索整个位置」不行。
    #[serde(default)]
    pub search: bool,
}

impl Capabilities {
    /// 本地文件系统：全能力。
    ///
    /// `search` 为 `false`：本地搜索是前端对已列出条目的过滤，没有「下推到
    /// 服务端」这回事。声明 `true` 会让界面承诺「搜索整个位置」，而实际上
    /// 搜不到没展开的子目录。
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
            search: false,
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
            search: false,
        }
    }

    /// 可写的云端位置（有真实路径层级的那种，如 WebDAV）。
    ///
    /// 注意 `random_write` 仍为 `false`：能上传整个文件，不等于能改其中
    /// 几个字节。`search` 也是 `false`：WebDAV 的 `SEARCH` 动词实现情况参差，
    /// 不能默认当它有。
    ///
    /// **这个构造器不适用于「对话即目录」的位置。** 那里 `rename` / `create_dir`
    /// 没有真实语义，硬套会让界面点亮两个必然失败的菜单项；用
    /// [`Capabilities::conversation_writable`]。
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
            search: false,
        }
    }

    /// 「对话即目录」的可写位置（Telegram 这一类）。
    ///
    /// 与 [`Capabilities::cloud_writable`] 的差别就是这个模型的全部特征，
    /// 逐条给出理由：
    ///
    /// - `write`：能往对话里发文件，但**受该对话的权限约束**——只读频道、
    ///   没有发文件权限的群里发不了。所以它由调用方按对话逐个传入，不是常量。
    /// - `delete`：能删自己发的消息（同样看对话权限），故也由调用方传入。
    /// - `rename` / `create_dir` 恒为 `false`：消息没有可改的路径，目录就是
    ///   对话列表本身，应用无从新建。**声明 `true` 会让界面点亮两个必然失败的
    ///   菜单项**——而 AGENTS.md 要求不支持就明确报不支持，不要静默降级。
    /// - `search` 为 `true`：服务端原生支持按关键词与媒体类型搜索，这是这类
    ///   位置相对 WebDAV 的主要优势。
    ///
    /// 注意 `search` 为真**不代表能搜到 omy 加密文件的真实名字**：那个名字只
    /// 存在于密文里，要让服务端搜到就得把明文文件名上传，等于把加密掉的信息
    /// 又主动交出去。搜的是消息文本与密文文件名。
    #[must_use]
    pub const fn conversation_writable(can_write: bool, can_delete: bool) -> Self {
        Self {
            read: true,
            write: can_write,
            delete: can_delete,
            rename: false,
            create_dir: false,
            random_write: false,
            range_read: true,
            search: true,
        }
    }

    /// 是否有任何写能力。界面据此决定整组写操作是否出现。
    ///
    /// **`search` 不算在内**：它是读能力。算进来会让一个只读的 Telegram 频道
    /// 因为「能搜」而被判成可写，整组写菜单都亮起来。
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

    /// 序列化字段名不能变，也不能漏。
    ///
    /// 不这样会怎样：前端读到 undefined，`!caps.write` 为真，
    /// 只读位置反而全部可写——这是最危险的失效方向。
    ///
    /// 新增字段必须同时加进下面这个列表：不加等于给自己留了个测不到的洞，
    /// 而这个测试存在的全部意义就是拦住「字段名与前端读取点不一致」。
    #[test]
    fn field_names_are_stable() {
        let j = serde_json::to_value(Capabilities::read_only()).expect("应可序列化");
        for k in [
            "read",
            "write",
            "delete",
            "rename",
            "create_dir",
            "random_write",
            "range_read",
            "search",
        ] {
            assert!(j.get(k).is_some(), "字段 {k} 不能改名或缺失");
        }
        assert_eq!(j["write"], serde_json::json!(false));
        // 字段数也要对上：多出一个没进列表的字段同样是协议漂移，
        // 而只检查「列出的都在」是抓不到的
        assert_eq!(
            j.as_object().map(serde_json::Map::len),
            Some(8),
            "字段数变了说明有字段没进上面的列表"
        );
    }

    /// 缺 `search` 的旧 JSON 必须仍能反序列化，且默认为 false。
    ///
    /// 不这样会怎样：`search` 是后加的字段，已落盘的配置和前端传回的旧对象里
    /// 都没有它。不带 `#[serde(default)]` 会让**整份能力位图**反序列化失败，
    /// 而失败之后位置要么用不了、要么退回默认值——那正是「读到 undefined、
    /// 只读位置全部可写」这条最危险的路径。
    #[test]
    fn missing_search_field_still_deserializes() {
        let old = serde_json::json!({
            "read": true,
            "write": false,
            "delete": false,
            "rename": false,
            "create_dir": false,
            "random_write": false,
            "range_read": true
        });
        let c: Capabilities = serde_json::from_value(old).expect("旧 JSON 必须仍能解析");
        assert!(!c.search, "缺字段时默认要是保守的 false");
        assert_eq!(c, Capabilities::read_only(), "其余字段要如实读出");
    }

    /// 本地是全能力，用于让本地位置走同一套判断而不必特例。
    #[test]
    fn local_is_fully_capable() {
        let c = Capabilities::local();
        assert!(c.any_write());
        assert!(c.random_write);
        assert!(c.range_read);
        assert!(
            !c.search,
            "本地搜索是前端过滤已列出的条目，不是服务端下推；声明 true 会让界面\
             承诺「搜索整个位置」而实际搜不到没展开的子目录"
        );
    }

    /// 「对话即目录」的位置必须如实声明没有改名与新建目录。
    ///
    /// 不这样会怎样：套用 `cloud_writable()` 会点亮「重命名」与「新建文件夹」
    /// 两个在这种位置上必然失败的菜单项——消息没有可改的路径，目录就是对话
    /// 列表本身。用户点下去只会收到一个没人看得懂的错误。
    #[test]
    fn conversation_place_has_no_rename_or_mkdir() {
        let c = Capabilities::conversation_writable(true, true);
        assert!(c.read);
        assert!(c.write, "有权限的对话能发文件");
        assert!(c.delete, "能删自己发的消息");
        assert!(!c.rename, "消息没有可改的路径，不能声明支持改名");
        assert!(!c.create_dir, "目录就是对话列表，应用无从新建");
        assert!(!c.random_write, "分片上传不等于能改其中几个字节");
        assert!(c.range_read, "分片下载能映射成 Range");
        assert!(c.search, "服务端原生支持搜索，这是这类位置的主要优势");

        // 与通用云端模板确实不同——若某次改动把两者变得一样，
        // 说明 rename/create_dir 又被硬套上去了
        assert_ne!(
            c,
            Capabilities::cloud_writable(),
            "对话型位置不能与通用云端位置声明相同的能力"
        );
    }

    /// 对话的写能力要跟着该对话的权限走，不能一律给真。
    ///
    /// 不这样会怎样：只读频道里「上传」是亮的，用户加密完、传到一半才收到
    /// 服务端拒绝——而这件事在点击之前就该知道。
    #[test]
    fn conversation_write_follows_per_chat_permission() {
        let ro = Capabilities::conversation_writable(false, false);
        assert!(ro.read, "只读频道仍然能看能下载");
        assert!(!ro.any_write(), "没有发文件权限的对话不能显示成可写");
        assert!(ro.search, "只读也能搜：搜索是读能力");

        // 只能删不能发，与只能发不能删，都是真实存在的组合
        let del_only = Capabilities::conversation_writable(false, true);
        assert!(del_only.any_write());
        assert!(!del_only.write);
        assert!(del_only.delete);
    }

    /// `search` 不能被算进写能力。
    ///
    /// 不这样会怎样：一个只读的 Telegram 频道因为「能搜」被判成可写，
    /// 整组写菜单都亮起来——而那是最危险的失效方向。
    #[test]
    fn search_is_not_a_write_capability() {
        let c = Capabilities {
            search: true,
            ..Capabilities::read_only()
        };
        assert!(!c.any_write(), "search 是读能力，绝不能让 any_write 变真");
    }
}
