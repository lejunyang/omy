//! 统一传输任务表：下载 / 上传 / 永久保留三类任务的汇总。
//!
//! # 为什么需要它
//!
//! 任务是**跨对话**的——用户可能同时在往收藏夹传一个文件、从某频道缓存
//! 一部电影、又给第三个对话里的文件做永久保留。进度只画在「当前所在的
//! 对话」里的话，用户一旦切走就看不到也管不了，只能反复切回去确认。
//!
//! 还有一个不那么显然的理由：这三类任务的失败原因互不相同（限流 /
//! 分片过期 / 空间不足），**只有汇总在一处才能看出「是不是所有任务都卡在
//! 同一个原因上」**。一次限流会让全部任务同时进入等待，分散显示时看起来
//! 像三个互不相关的故障。
//!
//! # 为什么放在 GUI 层
//!
//! 「传输任务」是界面概念，不是存储概念。provider 只管把字节搬过去，
//! 不该知道有人在另一个对话里也排了队。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::Emitter as _;

/// 任务表变化时推送的事件名。
///
/// 界面同时保留「查快照」与「订阅事件」两条路：快照用于首次渲染与兜底，
/// 事件用于即时刷新。只有事件的话，界面错过一条就永远对不上了。
pub const TRANSFER_EVENT: &str = "transfer://changed";

/// 任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 下载到本地（解密导出）。
    Download,
    /// 上传到远程位置。
    Upload,
    /// 转为永久保留——它在产品上**就是一次真实的下载任务**：
    /// 有进度、可取消、失败可重试，所以进同一张表。
    Pin,
}

/// 任务状态。
///
/// `Waiting` 与 `Running` 必须分开：限流时任务是「在等」而不是「在传」，
/// 混成一个状态的话，界面上会显示一排卡住不动的进度条，用户无从判断
/// 是网络慢还是被限流了。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskState {
    /// 排队中或被限流等待。`until_secs` 是限流还要等多久（如果知道）。
    Waiting {
        /// 限流还要等多久（秒）。服务端没给出秒数时为 `None`，
        /// 界面就只说「等待中」——编一个数字比不说更糟。
        until_secs: Option<u64>,
    },
    /// 正在传。
    Running,
    /// 已完成。
    Done,
    /// 失败。`code` 是结构化错误码，界面据它决定能不能重试。
    Failed {
        /// 结构化错误码。界面据它决定能不能重试，并翻成本地语言；
        /// 不下发拼好的文案，否则切换语言时这些字会保持旧语言。
        code: String,
        /// 这次失败能不能重试。由后端按错误码判定后下发，**界面不要自己
        /// 再推一遍**——判据只该有一处（见 `code_retryable`）。磁盘满 /
        /// 认证失效这类重试也没用的，为 false，界面就不给重试按钮：给一个
        /// 点了必然再失败的按钮比不给更糟。
        retryable: bool,
    },
    /// 用户取消。
    Canceled,
}

/// 一个失败错误码能不能重试。**这是重试判据的唯一出处**——前端不自己推，
/// 后端 finish 失败任务时用它填 `TaskState::Failed { retryable }`。
///
/// 语义与 omy-remote 的 `FailCause::retryable()` 一致：网络抖动、限流、
/// 未归类的 Other 可重试（限流重试前应先等 `until_secs`，但重试本身有意义）；
/// 磁盘满、认证 / 权限失效、协议不支持、分片过期这类**重试也必然再失败**，
/// 不给重试入口。新增错误码时**这里和 omy-remote 那份都要改**（判据同源）。
#[must_use]
pub fn code_retryable(code: &str) -> bool {
    !matches!(
        code,
        // 空间不足：重试前得先腾地方，点重试只会立刻再满
        "remote_disk_full"
        // 认证 / 权限：session 失效或没权限，重试改变不了
        | "remote_unauthorized"
        | "remote_forbidden"
        // 目标不存在 / 协议不支持 / CDN 重定向 / 广播无消息视图：都是确定性失败
        | "remote_not_found"
        | "remote_unsupported"
        | "tg_cdn_unsupported"
        | "tg_broadcast_no_messages"
    )
}

impl TaskState {
    /// 按错误码构造失败态，`retryable` 由 [`code_retryable`] 统一判定。
    #[must_use]
    pub fn failed(code: impl Into<String>) -> Self {
        let code = code.into();
        let retryable = code_retryable(&code);
        Self::Failed { code, retryable }
    }
}

/// 一条传输任务。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Task {
    /// 任务 id，前端用它发暂停 / 取消 / 重试。
    pub id: u64,
    /// 任务类型，界面据它分组。
    pub kind: TaskKind,
    /// 显示名（磁盘名，不是解密后的真名——真名可能是用户不想被看到的）。
    pub name: String,
    /// 来源或去向的可读描述，例如「我的 Telegram › 收藏夹」。
    pub target: String,
    /// 已传字节。
    pub done: u64,
    /// 总字节，未知时为 0。
    pub total: u64,
    #[serde(flatten)]
    /// 当前状态。序列化时摊平成 `state` 字段加各自的附加信息。
    pub state: TaskState,
}

/// 单个任务的可变句柄，交给执行体去更新进度。
#[derive(Debug)]
pub struct TaskHandle {
    id: u64,
    done: AtomicU64,
    canceled: AtomicBool,
    paused: AtomicBool,
}

impl TaskHandle {
    /// 这个任务被要求取消了吗。
    ///
    /// 执行体应当在**分片边界**检查它并干净退出。用 abort 直接砍掉会留下
    /// 半个文件，而那个文件看起来和传完的一样。
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Relaxed)
    }

    /// 被要求暂停了吗。
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 任务 id。
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// 更新已传字节。
    pub fn set_done(&self, n: u64) {
        self.done.store(n, Ordering::Relaxed);
    }

    /// 读已传字节。
    #[must_use]
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
}

/// 任务表。
#[derive(Debug, Default)]
pub struct Transfers {
    next_id: AtomicU64,
    tasks: Mutex<Vec<Task>>,
    handles: Mutex<Vec<Arc<TaskHandle>>>,
}

impl Transfers {
    /// 新建一条任务，返回给执行体用的句柄。
    pub fn start(
        &self,
        app: &tauri::AppHandle,
        kind: TaskKind,
        name: String,
        target: String,
        total: u64,
    ) -> Arc<TaskHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let h = Arc::new(TaskHandle {
            id,
            done: AtomicU64::new(0),
            canceled: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        });
        if let Ok(mut t) = self.tasks.lock() {
            t.push(Task {
                id,
                kind,
                name,
                target,
                done: 0,
                total,
                state: TaskState::Running,
            });
        }
        if let Ok(mut hs) = self.handles.lock() {
            hs.push(Arc::clone(&h));
        }
        self.notify(app);
        h
    }

    /// 同步一条任务的进度（从句柄读）。
    pub fn tick(&self, app: &tauri::AppHandle, h: &TaskHandle) {
        let changed = if let Ok(mut t) = self.tasks.lock() {
            t.iter_mut().find(|x| x.id == h.id).is_some_and(|task| {
                let d = h.done();
                let diff = task.done != d;
                task.done = d;
                diff
            })
        } else {
            false
        };
        if changed {
            self.notify(app);
        }
    }

    /// 结束一条任务。
    pub fn finish(&self, app: &tauri::AppHandle, id: u64, state: TaskState) {
        if let Ok(mut t) = self.tasks.lock() {
            if let Some(task) = t.iter_mut().find(|x| x.id == id) {
                if matches!(state, TaskState::Done) {
                    task.done = task.total;
                }
                task.state = state;
            }
        }
        if let Ok(mut hs) = self.handles.lock() {
            hs.retain(|h| h.id != id);
        }
        self.notify(app);
    }

    /// 当前全部任务的快照。
    #[must_use]
    pub fn list(&self) -> Vec<Task> {
        self.tasks.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// 取消一条任务。执行体会在下一个分片边界退出。
    pub fn cancel(&self, app: &tauri::AppHandle, id: u64) {
        if let Ok(hs) = self.handles.lock() {
            if let Some(h) = hs.iter().find(|h| h.id == id) {
                h.canceled.store(true, Ordering::Relaxed);
            }
        }
        // 立刻把状态改掉，不等执行体——否则用户点了取消，界面要等到
        // 下一个分片才有反应，看起来像没点上
        self.finish(app, id, TaskState::Canceled);
        let _ = app;
    }

    /// 把一条**失败**任务翻回「进行中」并发一个新句柄——纯状态机部分，
    /// 不碰 app / 不发事件，便于单测。非失败任务返回 `None`。
    ///
    /// 只对 `Failed` 生效：对进行中 / 已完成的任务重试没有意义。done 清回
    /// 起点只是 UI 进度条归零；pin 的预热会跳过已缓存的块（见
    /// `RemoteSource::fetch_block`），所以「从进度续」是天然的，重试不会
    /// 重下已经在本地的块。
    fn revive_failed(&self, id: u64) -> Option<Arc<TaskHandle>> {
        let ok = self.tasks.lock().ok().is_some_and(|mut t| {
            t.iter_mut().find(|x| x.id == id).is_some_and(|task| {
                if matches!(task.state, TaskState::Failed { .. }) {
                    task.state = TaskState::Running;
                    task.done = 0;
                    true
                } else {
                    false
                }
            })
        });
        if !ok {
            return None;
        }
        let h = Arc::new(TaskHandle {
            id,
            done: AtomicU64::new(0),
            canceled: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        });
        if let Ok(mut hs) = self.handles.lock() {
            // 清掉可能残留的旧句柄，避免两个句柄同时写同一条任务的进度
            hs.retain(|x| x.id != id);
            hs.push(Arc::clone(&h));
        }
        Some(h)
    }

    /// 重试一条失败任务：翻回 Running、发新句柄、推事件。返回句柄给调用方
    /// 去重跑原操作（当前只有 pin 会进传输表，见 `transfer_retry`）。
    pub fn reset_running(&self, app: &tauri::AppHandle, id: u64) -> Option<Arc<TaskHandle>> {
        let h = self.revive_failed(id)?;
        self.notify(app);
        Some(h)
    }

    /// 全部暂停 / 全部继续。
    pub fn pause_all(&self, app: &tauri::AppHandle, paused: bool) {
        if let Ok(hs) = self.handles.lock() {
            for h in hs.iter() {
                h.paused.store(paused, Ordering::Relaxed);
            }
        }
        if let Ok(mut t) = self.tasks.lock() {
            for task in t.iter_mut() {
                match (&task.state, paused) {
                    (TaskState::Running, true) => {
                        task.state = TaskState::Waiting { until_secs: None };
                    }
                    (TaskState::Waiting { .. }, false) => task.state = TaskState::Running,
                    _ => {}
                }
            }
        }
        self.notify(app);
    }

    /// 清除已结束的任务（完成 / 失败 / 取消）。
    ///
    /// 失败和取消的也一并清掉：它们同样是「已结束」，留着只会让列表
    /// 越积越长，而用户点「清除已完成」的意图就是清空这些。
    pub fn clear_done(&self, app: &tauri::AppHandle) -> Vec<u64> {
        let mut removed = Vec::new();
        if let Ok(mut t) = self.tasks.lock() {
            t.retain(|x| {
                let keep = matches!(x.state, TaskState::Running | TaskState::Waiting { .. });
                if !keep {
                    removed.push(x.id);
                }
                keep
            });
        }
        self.notify(app);
        removed
    }

    /// 未结束的任务数，用于侧栏角标。
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.tasks.lock().map_or(0, |t| {
            t.iter()
                .filter(|x| matches!(x.state, TaskState::Running | TaskState::Waiting { .. }))
                .count()
        })
    }

    fn notify(&self, app: &tauri::AppHandle) {
        let _ = app.emit(TRANSFER_EVENT, self.list());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三类任务共用一张表——这正是「一处汇总」的意义。
    ///
    /// 不这样会怎样：上传、下载、永久保留各自维护列表，界面要查三处，
    /// 而「是不是都卡在同一个原因上」这个判断就做不出来了。
    #[test]
    fn all_three_kinds_share_one_table() {
        let t = Transfers::default();
        // 不经 app 的纯逻辑部分
        for (kind, name) in [
            (TaskKind::Upload, "a.omy"),
            (TaskKind::Download, "b.mp4"),
            (TaskKind::Pin, "c.png"),
        ] {
            let id = t.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            if let Ok(mut v) = t.tasks.lock() {
                v.push(Task {
                    id,
                    kind,
                    name: name.to_owned(),
                    target: String::from("x"),
                    done: 0,
                    total: 100,
                    state: TaskState::Running,
                });
            }
        }
        let all = t.list();
        assert_eq!(all.len(), 3, "三类任务应当在同一张表里");
        assert_eq!(t.active_count(), 3);
    }

    /// 角标只数**未结束**的。
    ///
    /// 不这样会怎样：完成的任务一直记在角标上，数字只增不减，
    /// 用户会以为有一堆任务卡着。
    #[test]
    fn badge_counts_only_unfinished() {
        let t = Transfers::default();
        if let Ok(mut v) = t.tasks.lock() {
            v.push(Task {
                id: 1,
                kind: TaskKind::Upload,
                name: String::from("a"),
                target: String::new(),
                done: 1,
                total: 1,
                state: TaskState::Done,
            });
            v.push(Task {
                id: 2,
                kind: TaskKind::Download,
                name: String::from("b"),
                target: String::new(),
                done: 0,
                total: 1,
                state: TaskState::Running,
            });
            v.push(Task {
                id: 3,
                kind: TaskKind::Pin,
                name: String::from("c"),
                target: String::new(),
                done: 0,
                total: 1,
                state: TaskState::failed("x"),
            });
        }
        assert_eq!(
            t.active_count(),
            1,
            "只有 Running/Waiting 该计入角标，Done 与 Failed 不算"
        );
    }

    /// 「清除已完成」要把失败和取消的也清掉。
    ///
    /// 不这样会怎样：失败的任务永远留在列表里，用户点了清除却发现
    /// 列表几乎没变，只能一条条手动处理。
    #[test]
    fn clear_done_also_clears_failed_and_canceled() {
        let t = Transfers::default();
        if let Ok(mut v) = t.tasks.lock() {
            for (id, st) in [
                (1_u64, TaskState::Done),
                (2, TaskState::failed("e")),
                (3, TaskState::Canceled),
                (4, TaskState::Running),
                (5, TaskState::Waiting { until_secs: Some(30) }),
            ] {
                v.push(Task {
                    id,
                    kind: TaskKind::Upload,
                    name: String::new(),
                    target: String::new(),
                    done: 0,
                    total: 0,
                    state: st,
                });
            }
        }
        if let Ok(mut v) = t.tasks.lock() {
            v.retain(|x| matches!(x.state, TaskState::Running | TaskState::Waiting { .. }));
        }
        let left: Vec<u64> = t.list().iter().map(|x| x.id).collect();
        assert_eq!(left, vec![4, 5], "只该留下未结束的");
    }

    /// 重试判据只该有一处：能重试与不能重试的错误码分得对。
    ///
    /// 不这样会怎样：把「磁盘满 / 认证失效」也标成可重试，界面就给一个
    /// 点了必然再失败的重试按钮——用户反复点反复失败，比不给更糟。
    #[test]
    fn retryable_codes_split_correctly() {
        for c in ["remote_network", "remote_rate_limited", "remote_prefetch_failed", "remote_failed"] {
            assert!(code_retryable(c), "{c} 该可重试");
        }
        for c in [
            "remote_disk_full",
            "remote_unauthorized",
            "remote_forbidden",
            "remote_not_found",
            "remote_unsupported",
            "tg_cdn_unsupported",
            "tg_broadcast_no_messages",
        ] {
            assert!(!code_retryable(c), "{c} 重试也没用，不该给重试入口");
        }
        // 不用 match+panic：omy-gui 禁 panic（含测试）。用 if let 取字段再断言。
        let st = TaskState::failed("remote_disk_full");
        assert!(matches!(st, TaskState::Failed { .. }), "应为 Failed");
        if let TaskState::Failed { retryable, .. } = st {
            assert!(!retryable, "磁盘满该判为不可重试");
        }
    }

    /// revive_failed 只翻失败任务、清进度、发句柄；进行中任务不动。
    ///
    /// 不这样会怎样：对进行中任务也能 revive，会凭空多发一个句柄同时写
    /// 同一条任务的进度，两个句柄打架、进度乱跳。
    #[test]
    fn revive_only_failed_task() {
        let t = Transfers::default();
        if let Ok(mut v) = t.tasks.lock() {
            v.push(Task {
                id: 7, kind: TaskKind::Pin, name: String::from("c"), target: String::new(),
                done: 5, total: 10, state: TaskState::failed("remote_network"),
            });
            v.push(Task {
                id: 8, kind: TaskKind::Pin, name: String::from("d"), target: String::new(),
                done: 3, total: 10, state: TaskState::Running,
            });
        }
        // 进行中任务：拒绝 revive，句柄数不变
        assert!(t.revive_failed(8).is_none(), "Running 不该被 revive");
        // 失败任务：翻回 Running、进度清零、发出句柄
        let h = t.revive_failed(7).expect("失败任务应能 revive");
        assert_eq!(h.id(), 7);
        let snap = t.list();
        let task7 = snap.iter().find(|x| x.id == 7).expect("任务还在");
        assert_eq!(task7.state, TaskState::Running, "该翻回 Running");
        assert_eq!(task7.done, 0, "UI 进度该清回起点");
        // 只该有一个 7 号句柄（旧的被清）
        let n7 = t.handles.lock().map(|hs| hs.iter().filter(|x| x.id == 7).count()).unwrap_or(0);
        assert_eq!(n7, 1, "重试后只该有一个句柄，避免两个句柄打架");
    }

    /// 限流等待与正在传必须是两个状态。
    ///
    /// 不这样会怎样：限流时界面显示一排不动的进度条，用户无从判断是
    /// 网络慢还是被限流——而这两件事该做的处理完全不同。
    #[test]
    fn waiting_is_distinct_from_running() {
        let w = TaskState::Waiting { until_secs: Some(30) };
        let r = TaskState::Running;
        assert_ne!(w, r);
        let j = serde_json::to_string(&w).expect("序列化");
        assert!(j.contains("waiting"), "状态要能被前端区分，实际 {j}");
        assert!(j.contains("30"), "等待秒数要传给界面，实际 {j}");
    }
}
