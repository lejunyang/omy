//! 连接时上报的设备标识（`initConnection` 的 device 参数）。
//!
//! # 为什么单独一个模块：单点定义
//!
//! 这几个字段在 `initConnection` 里要一起给，将来若因风控被迫调整，得能一处
//! 改完。散在连接、登录两处必然漏一个，而漏掉的那处会让一部分会话上报不一致
//! 的身份——现象是「有些设备在已登录列表里显示得不一样」，极难归因。
//!
//! # 为什么如实报 omy，而不是伪装成 Telegram Desktop
//!
//! `device_model` 会显示在官方客户端的**「已登录设备」列表**里。这是一个决定
//! 产品立场的地方：
//!
//! - 报 `omy`：用户在那个列表里**能认出这是哪个程序，也就能撤销它**。
//! - 伪装成 Telegram Desktop：用户**无法把这个会话与自己真正的桌面端区分开**，
//!   想撤销都不知道该撤哪一个。
//!
//! 对一个做加密与隐私的工具来说，后者说不过去。内置官方 api_id 已经是「借用
//! 应用身份」了（那是为了让用户装上就能用），再对齐 device 就变成「主动隐藏
//! 自己是第三方客户端」——那是另一件事，而且是对用户隐藏，不是对服务端。
//!
//! # 一个未核实的风险，记在这里
//!
//! 「用官方 api_id 但报第三方 device 会不会更容易触发风控」**没有任何官方说明
//! 或可靠案例**。所以上面是一个产品立场上的选择，不是有证据的安全结论。将来
//! 若出现证据，改动只在这个文件里——这也正是它单独成模块的理由之一。

/// 上报给服务端的设备标识。
///
/// 字段与 `initConnection` 的参数一一对应。**不含任何凭据**，所以可以放心
/// 派生 `Debug`、写进日志——这与 [`super::appid::AppId`] 刻意相反，那边有
/// `api_hash` 所以必须手写 `Debug`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// 显示在官方客户端「已登录设备」列表里的那一行。
    ///
    /// 必须让用户认得出来，理由见模块注释。
    pub device_model: String,
    /// 操作系统版本。
    pub system_version: String,
    /// 应用版本。
    pub app_version: String,
    /// 系统语言（如 `zh`）。
    pub system_lang_code: String,
    /// 界面语言（如 `zh`）。
    pub lang_code: String,
}

/// 「已登录设备」列表里显示的名字。
///
/// **单点定义**：改这里就够了。
///
/// 为什么带上程序名而不只报系统信息：用户在官方客户端里看到的就是这一行，
/// 只报 "Windows" 的话他分不清是哪个程序连上来的，也就不知道该不该撤销。
pub const DEVICE_MODEL: &str = "omy";

/// 没能探测到系统版本时用的占位值。
///
/// 报一个明确的占位比报空字符串好：空字符串在「已登录设备」列表里会显示成
/// 一片空白，看起来像出错了。
const UNKNOWN_SYSTEM: &str = "unknown";

impl DeviceInfo {
    /// 按当前编译目标与运行环境组装。
    ///
    /// `app_version` 取 crate 版本，这样用户报问题时那一行就能对上版本。
    #[must_use]
    pub fn current() -> Self {
        Self {
            device_model: String::from(DEVICE_MODEL),
            system_version: detect_system_version(),
            app_version: String::from(env!("CARGO_PKG_VERSION")),
            system_lang_code: String::from("en"),
            lang_code: String::from("en"),
        }
    }

    /// 换掉界面语言。
    ///
    /// 语言由上层（GUI 的 i18n）决定，这一层拿不到，所以留一个入口而不是在
    /// 这里猜。
    #[must_use]
    pub fn with_lang(mut self, lang: &str) -> Self {
        if !lang.is_empty() {
            self.lang_code = String::from(lang);
            self.system_lang_code = String::from(lang);
        }
        self
    }
}

/// 探测系统版本字符串。
///
/// 只用 `cfg`，不调系统 API：这个值只是显示给用户看的一行字，为它引入平台
/// 相关的探测代码（以及 Android 上那些不存在的命令）不值得。
fn detect_system_version() -> String {
    String::from(os_name(Platform {
        android: cfg!(target_os = "android"),
        windows: cfg!(target_os = "windows"),
        macos: cfg!(target_os = "macos"),
        ios: cfg!(target_os = "ios"),
        // 故意用 `unix` 而不是 `target_os = "linux"` 来兜底，这样 BSD 之类
        // 也能得到一个像样的名字而不是 unknown。代价是它**在 Android 上也为
        // 真**，所以下面的判断顺序有讲究，见 `os_name`。
        unix: cfg!(unix),
    }))
}

/// 当前编译目标命中了哪些 `cfg`。
///
/// 把 `cfg!` 的取值收进一个结构体、判断逻辑单独成函数，是为了让那段顺序敏感
/// 的判断能被真正测到。直接在函数体里写 `cfg!` 的话，一次编译只会走其中一条
/// 分支——「Android 被归成 Linux」这种错误在 Windows 上编译时**根本不可观测**，
/// 单测写了也是假的。
struct Platform {
    android: bool,
    windows: bool,
    macos: bool,
    ios: bool,
    unix: bool,
}

/// 把命中的 `cfg` 映射成显示用的系统名。
///
/// **`android` 必须排在 `unix` 之前**：Android 也是 unix，顺序颠倒会让所有
/// 安卓用户在「已登录设备」列表里看到 Linux。这正是 AGENTS.md 记的那个坑
/// （`cfg(all(unix, not(target_os = "macos")))` 在 Android 上同样成立）。
const fn os_name(p: Platform) -> &'static str {
    if p.android {
        "Android"
    } else if p.windows {
        "Windows"
    } else if p.macos {
        "macOS"
    } else if p.ios {
        "iOS"
    } else if p.unix {
        "Linux"
    } else {
        UNKNOWN_SYSTEM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设备名必须是用户能认出来的 omy，不能伪装成官方客户端。
    ///
    /// 不这样会怎样：这一行会显示在官方客户端的「已登录设备」列表里，写成
    /// Telegram Desktop 的话用户分不清哪个是自己真正的桌面端、哪个是 omy，
    /// 想撤销都不知道撤哪一个——对一个做隐私的工具来说说不过去。
    #[test]
    fn device_model_is_honest_about_being_omy() {
        let d = DeviceInfo::current();
        assert_eq!(d.device_model, "omy");

        let lowered = d.device_model.to_ascii_lowercase();
        assert!(
            !lowered.contains("telegram") && !lowered.contains("desktop"),
            "不能伪装成官方客户端: {}",
            d.device_model
        );
    }

    /// 没有一个字段是空的。
    ///
    /// 不这样会怎样：空字段在「已登录设备」列表里显示成一片空白，用户会以为
    /// 出错了；而服务端对这些参数也并非全都接受空值。
    #[test]
    fn no_field_is_left_empty() {
        let d = DeviceInfo::current();
        for (name, v) in [
            ("device_model", &d.device_model),
            ("system_version", &d.system_version),
            ("app_version", &d.app_version),
            ("system_lang_code", &d.system_lang_code),
            ("lang_code", &d.lang_code),
        ] {
            assert!(!v.is_empty(), "{name} 不能是空的");
        }
    }

    /// 版本号取自 crate，不是写死的。
    ///
    /// 不这样会怎样：写死的版本号会在发版后变成错的，而用户报问题时我们正是
    /// 靠「已登录设备」里那一行判断他用的哪一版。
    #[test]
    fn app_version_tracks_the_crate() {
        assert_eq!(DeviceInfo::current().app_version, env!("CARGO_PKG_VERSION"));
    }

    /// 换语言要同时改两个语言字段，空字符串不覆盖。
    ///
    /// 不这样会怎样：只改 lang_code 会让两个字段互相矛盾；而用空字符串覆盖
    /// 等于把字段清空，又回到上面那条「显示成空白」的问题。
    #[test]
    fn with_lang_sets_both_and_ignores_empty() {
        let d = DeviceInfo::current().with_lang("zh");
        assert_eq!(d.lang_code, "zh");
        assert_eq!(d.system_lang_code, "zh", "两个语言字段必须一致");

        let keep = DeviceInfo::current().with_lang("zh").with_lang("");
        assert_eq!(keep.lang_code, "zh", "空字符串不该把已设好的语言清掉");
        assert!(!keep.system_lang_code.is_empty());
    }

    /// 本机这次编译必须落在一个已知的系统名上。
    ///
    /// 不这样会怎样：cfg 链写漏一个分支就会静默落到 unknown，而这在本机跑
    /// 起来一切正常，只有用户的「已登录设备」里显示 unknown 才暴露。
    #[test]
    fn system_version_is_detected_on_this_target() {
        let v = detect_system_version();
        assert_ne!(v, UNKNOWN_SYSTEM, "本机的目标平台应当被识别出来: {v}");
        let expected = if cfg!(target_os = "android") {
            "Android"
        } else if cfg!(target_os = "windows") {
            "Windows"
        } else if cfg!(target_os = "macos") {
            "macOS"
        } else if cfg!(unix) {
            "Linux"
        } else {
            "iOS"
        };
        assert_eq!(v, expected);
    }

    /// Android 也是 unix，必须先判 Android。
    ///
    /// 不这样会怎样：所有安卓用户在官方客户端的「已登录设备」列表里会看到
    /// Linux。而这个错误**在 Windows 上编译时根本不可观测**——一次编译只走
    /// 一条分支，所以必须像这里一样把 cfg 的取值喂进纯函数才测得到。
    /// AGENTS.md 记的正是这个坑：`cfg(all(unix, not(target_os = "macos")))`
    /// 在 Android 上同样成立。
    #[test]
    fn android_wins_over_the_unix_fallback() {
        let android = Platform {
            android: true,
            windows: false,
            macos: false,
            ios: false,
            // Android 上这一位确实是真的，所以必须构造成 true 才算测到
            unix: true,
        };
        assert_eq!(os_name(android), "Android");

        // 真正的 Linux 才该落到兜底那一支
        let linux = Platform {
            android: false,
            windows: false,
            macos: false,
            ios: false,
            unix: true,
        };
        assert_eq!(os_name(linux), "Linux");
    }

    /// 每个平台各归各位，且全不认识时才落 unknown。
    ///
    /// 不这样会怎样：判断链里少一个 else 或写错一个分支，会让某个平台悄悄
    /// 显示成另一个平台的名字——本机编译一切正常，只有那个平台的用户看得到。
    #[test]
    fn each_platform_maps_to_its_own_name() {
        let base = Platform {
            android: false,
            windows: false,
            macos: false,
            ios: false,
            unix: false,
        };
        assert_eq!(os_name(Platform { windows: true, ..base }), "Windows");
        // macOS 与 iOS 也都是 unix，一并构造成 true 才算测到顺序
        assert_eq!(
            os_name(Platform {
                macos: true,
                unix: true,
                ..base
            }),
            "macOS"
        );
        assert_eq!(
            os_name(Platform {
                ios: true,
                unix: true,
                ..base
            }),
            "iOS"
        );
        assert_eq!(os_name(base), UNKNOWN_SYSTEM, "全不认识时才该落 unknown");
    }
}
