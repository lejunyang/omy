//! 传输任务模型：任务列表、进度、暂停 / 取消 / 重试的状态机。
//!
//! 口径来自 `docs/research/appendix/telegram-remote-prototype.html` 第 4b 节与
//! 末尾那张「与连接状态机**正交**的传输任务状态机」。**这一层不碰网络**：它只
//! 决定「现在允许做什么、下一步是什么状态」，字节搬运由各 provider 负责。
//!
//! # 为什么先做纯状态机
//!
//! 这些规则里最容易写错的几条，都与具体协议无关：
//!
//! - **「等待中」必须与「失败」分开。** 限流等待会自动恢复，画成失败会让用户
//!   去点重试——而重试只会把等待时间越点越长。
//! - **「排队中」不是 0%。** 达到并发上限前显示 0% 进度条，用户会以为卡死了。
//! - **失败可不可以重试，以及重试从哪开始，是两个独立的问题。** 分片过期在
//!   服务端只暂存几分钟到几小时，跨天续传做不到，必须如实报「需从头重传」；
//!   而网络断开是可以续的。混成一个 `retryable` 布尔位，就会出现「点了重试、
//!   进度从 71% 继续、然后再失败一次」的循环。
//! - **磁盘满不该给重试按钮。** 空间没腾出来之前，重试必然再失败一次。
//!
//! 把它们做成不依赖协议的状态机，才能在没有真实账号、没有网络的条件下用单测
//! 逐条钉住。接上 provider 之后这些规则不会改。
//!
//! # 与永久缓存的关系
//!
//! 「转为永久」在产品语义上就是**一次真实的下载任务**（`TransferKind::Pin`），
//! 有进度、能取消、失败能重试——不是一个点完就静默在后台跑的开关，那会让用户
//! 以为已经能离线打开了。所以它与上传、下载共用这同一个模型，而不是另做一套。
//! 缓存侧的落盘规则见 [`crate::cache`]（标记先写、之后下到的块直接进永久层）。

use std::collections::HashMap;

use serde::Serialize;

/// 任务标识。
///
/// 用不透明的自增 id 而不是文件路径：汇总视图里同名文件可能来自不同对话，
/// 按名字找任务会命中错的那条——表现是「点了取消，别人的任务停了」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct TaskId(pub u64);

/// 任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    /// 加密后上传。
    Upload,
    /// 解密到本地。
    Download,
    /// 转为永久缓存（把整个文件的密文块下下来并留住）。
    Pin,
}

/// 任务为什么在等待。
///
/// 等待**不是失败**，所以它有自己的类型：界面要照实写出原因，不能只显示一个
/// 停住的进度条。用户看不到原因就会以为任务坏了而反复重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum WaitReason {
    /// 被服务端限流，剩余秒数由 [`TransferManager::tick`] 递减。
    ///
    /// 这段时间里**相关按钮应当禁用**：让按钮可点然后必然失败等于骗用户，
    /// 而在限流期间重试会把等待时间越点越长。
    RateLimited {
        /// 还需等待的秒数。
        secs: u32,
    },
}

/// 重试要从哪里开始。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resume {
    /// 可以接着已有进度继续。
    FromProgress,
    /// 必须从头再来，已有进度作废。
    ///
    /// 上传分片在服务端只暂存几分钟到几小时，跨天续传做不到。把这种情况显示成
    /// 「从 71% 继续」是骗人的：点下去还是会失败，用户只会反复点。
    FromStart,
}

/// 失败原因。
///
/// 每一项都要自己回答两个问题：**能不能重试**、**重试从哪开始**。合成一个
/// 布尔位会让「磁盘满」也长出一个点了必然再失败的重试按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "cause")]
pub enum FailCause {
    /// 网络不可达或超时。可重试，且能接着已有进度。
    Network,
    /// 服务端的分片 / 文件引用已过期。可重试，但**必须从头开始**。
    Expired,
    /// 磁盘空间不足。**不可重试**，直到用户腾出空间。
    DiskFull,
    /// 认证失效（会话被作废等）。**不可重试**，要重新登录。
    ///
    /// 当成可重试错误会让界面无限转圈，而真正要做的是弹重新登录。
    Unauthorized,
    /// 位置不支持该操作。不可重试——这是 bug，不是用户能解决的问题。
    Unsupported,
    /// 其它本地或协议错误。可重试，接着已有进度。
    Other,
}

impl FailCause {
    /// 能不能重试。
    #[must_use]
    pub const fn retryable(self) -> bool {
        match self {
            Self::Network | Self::Expired | Self::Other => true,
            Self::DiskFull | Self::Unauthorized | Self::Unsupported => false,
        }
    }

    /// 重试从哪开始。
    #[must_use]
    pub const fn resume(self) -> Resume {
        match self {
            Self::Expired => Resume::FromStart,
            _ => Resume::FromProgress,
        }
    }
}

impl From<&crate::Error> for FailCause {
    /// 把 provider 的错误映射成失败原因。
    ///
    /// 由这里统一映射，而不是每个 provider 各自判断：判断散落之后，
    /// 新 provider 很容易把「会话被作废」也归成可重试，于是界面无限转圈，
    /// 而真正该做的是弹重新登录。
    fn from(e: &crate::Error) -> Self {
        match e {
            crate::Error::Network(_) => Self::Network,
            crate::Error::Unauthorized | crate::Error::Forbidden => Self::Unauthorized,
            crate::Error::Unsupported(_) => Self::Unsupported,
            // 限流**不是失败**，不该走到这里：它要变成 Waiting，由
            // TransferManager::rate_limited 处理。映射成可重试的 Other 是
            // 兜底，真正的修法是调用方别把它当错误报上来
            crate::Error::RateLimited => Self::Other,
            crate::Error::NotFound(_) | crate::Error::Protocol(_) => Self::Other,
            crate::Error::Io(io) => {
                // 磁盘满与普通 I/O 错误的处置完全相反（一个不给重试按钮、
                // 一个给），所以必须分开认
                if io.kind() == std::io::ErrorKind::StorageFull {
                    Self::DiskFull
                } else {
                    Self::Other
                }
            }
        }
    }
}

/// 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TaskState {
    /// 排队中：并发已满，还没轮到。
    ///
    /// 界面要显示「排队中」而**不是 0%**：一个停在 0 的进度条看起来就是卡死了。
    Queued,
    /// 进行中。
    Running,
    /// 用户暂停。
    Paused,
    /// 等待中（**不是失败**）。
    Waiting(WaitReason),
    /// 失败。
    Failed(FailCause),
    /// 用户取消。
    Cancelled,
    /// 已完成。
    Done,
}

impl TaskState {
    /// 是否为终态（不会再自行变化）。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Done)
    }

    /// 是否占用一个并发额度。
    ///
    /// **等待中也占额度**，这是刻意的：限流是账号级别的，此时放另一个任务进来
    /// 只会一起撞上限流，还让风控看到更多请求。把额度让出去等于「一个任务被限流，
    /// 于是我们再开一个去挨同样的打」。
    #[must_use]
    pub const fn holds_slot(self) -> bool {
        matches!(self, Self::Running | Self::Waiting(_))
    }
}

/// 一条传输任务。
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    /// 任务标识。
    pub id: TaskId,
    /// 任务类型。
    pub kind: TransferKind,
    /// 位置标识（哪个远程位置）。
    pub place: String,
    /// 条目标识。
    ///
    /// 与来源信息一起构成汇总视图里那行「← 我的 Telegram › 某频道 · 消息 #9021」。
    /// **这一行是必需的**：同名文件可能来自不同对话，不写来源用户无法判断要取消
    /// 的是哪一个。
    pub entry_id: String,
    /// 展示名。
    ///
    /// 由调用方给：加密文件的真实名字要解密才知道，这一层不该、也无法自己算。
    pub label: String,
    /// 总字节数；`0` 表示尚未可知。
    pub total_bytes: u64,
    /// 已完成字节数。
    pub done_bytes: u64,
    /// 当前状态。
    pub state: TaskState,
}

impl Task {
    /// 进度百分比（0–100）。总量未知时返回 `None`——**不要返回 0**。
    ///
    /// 返回 0 会和「排队中」画成同一个样子，而两者对用户的含义完全不同：
    /// 一个是还没开始，一个是不知道多大。
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        if self.total_bytes == 0 {
            return None;
        }
        let p = self.done_bytes.saturating_mul(100) / self.total_bytes;
        u8::try_from(p.min(100)).ok()
    }

    /// 界面上能不能给这条任务一个「重试」按钮。
    ///
    /// 只有**可重试的失败**才行。限流等待不算（重试只会更糟）、磁盘满不算
    /// （空间没腾出来前必然再失败）。
    #[must_use]
    pub const fn can_retry(&self) -> bool {
        match self.state {
            TaskState::Failed(c) => c.retryable(),
            _ => false,
        }
    }

    /// 永久缓存的 📌 角标能不能显示了。
    ///
    /// **只有下载完成之后**。钉住的那一刻就显示角标，用户会以为已经能离线打开了
    /// ——而此时文件还在下载，断网就打不开。
    #[must_use]
    pub const fn pin_badge_visible(&self) -> bool {
        matches!(self.kind, TransferKind::Pin) && matches!(self.state, TaskState::Done)
    }
}

/// 取消一条任务后，已经落盘的那部分要怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCleanup {
    /// 保留已下载的部分。
    ///
    /// 普通下载 / 上传取消后，已缓存的密文块留着没坏处：它们本来就是可被 LRU
    /// 回收的临时块，下次再看还能省流量。
    KeepPartial,
    /// 丢弃已下载的部分。
    ///
    /// 永久缓存任务取消后**不留半成品**：那些块在永久层，既不计入上限也不会被
    /// 淘汰，留下来就是一份永远不会被清理、用户也看不到的垃圾。
    DropPartial,
}

/// 状态机拒绝了某个操作。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    /// 没有这条任务。
    #[error("没有这条传输任务")]
    NoSuchTask,
    /// 当前状态不允许这个操作。
    ///
    /// 走到这里说明界面没按状态过滤按钮，或者两处对状态的判断不一致——
    /// 与 [`crate::Capabilities`] 那条「唯一真相」是同一类问题。
    #[error("当前状态不允许该操作")]
    InvalidTransition,
}

/// 任务列表的汇总计数，供状态栏显示。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TransferSummary {
    /// 进行中。
    pub running: u32,
    /// 排队中。
    pub queued: u32,
    /// 等待中（限流等）。**与失败分开计**。
    pub waiting: u32,
    /// 已暂停。
    pub paused: u32,
    /// 失败。
    pub failed: u32,
    /// 已完成。
    pub done: u32,
    /// 已取消。
    pub cancelled: u32,
}

/// 默认并发任务数。
///
/// 与 `Remote::scan_concurrency` 同一性质：**这不是性能调参，是风控边界**。
/// 传输比扫描更重，所以取更小的值——调高容易被服务端限流甚至封禁，而封禁是
/// 账号级别的后果。
pub const DEFAULT_MAX_RUNNING: usize = 3;

/// 传输任务管理器。
///
/// 纯内存、纯同步：它只管状态，不持有网络连接也不起线程。这样才能在单测里把
/// 每条规则逐个钉住，而不必造一个假服务端。
#[derive(Debug)]
pub struct TransferManager {
    tasks: Vec<Task>,
    next_id: u64,
    max_running: usize,
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RUNNING)
    }
}

impl TransferManager {
    /// 建一个空的管理器。
    ///
    /// `max_running` 为 0 时按 1 处理：0 会让所有任务永远排队，界面上表现为
    /// 「点了下载但什么也不发生」，而这种配置错误没有任何提示。
    #[must_use]
    pub fn new(max_running: usize) -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
            max_running: max_running.max(1),
        }
    }

    /// 并发上限。
    #[must_use]
    pub fn max_running(&self) -> usize {
        self.max_running
    }

    /// 调整并发上限，并立即按新上限补跑排队中的任务。
    pub fn set_max_running(&mut self, n: usize) {
        self.max_running = n.max(1);
        self.pump();
    }

    /// 全部任务，按加入顺序。
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// 按 id 取任务。
    #[must_use]
    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// 加入一条任务。返回它的 id。
    ///
    /// 新任务一律先进 [`TaskState::Queued`]，再由 [`TransferManager::pump`] 按并发
    /// 上限提升为 `Running`。**不直接置为 Running**：那样上限就形同虚设，而超过
    /// 上限的并发请求正是触发风控的路径。
    pub fn enqueue(
        &mut self,
        kind: TransferKind,
        place: impl Into<String>,
        entry_id: impl Into<String>,
        label: impl Into<String>,
        total_bytes: u64,
    ) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        self.tasks.push(Task {
            id,
            kind,
            place: place.into(),
            entry_id: entry_id.into(),
            label: label.into(),
            total_bytes,
            done_bytes: 0,
            state: TaskState::Queued,
        });
        self.pump();
        id
    }

    /// 按并发上限把排队中的任务提升为进行中。
    ///
    /// 每次状态变化后都要调用：任务一完成就该有下一条顶上，否则用户会看到
    /// 「剩下的全在排队，但一个都不动」。
    pub fn pump(&mut self) {
        let mut busy = self.tasks.iter().filter(|t| t.state.holds_slot()).count();
        for t in &mut self.tasks {
            if busy >= self.max_running {
                break;
            }
            if matches!(t.state, TaskState::Queued) {
                t.state = TaskState::Running;
                busy += 1;
            }
        }
    }

    /// 报告进度。
    ///
    /// 只在 `Running` 状态下接受：任务已被用户暂停 / 取消之后，在途的回调仍可能
    /// 迟到几百毫秒，照收会让一条「已暂停」的任务进度自己往前跳，或者让一条已取消
    /// 的任务复活。返回是否被接受。
    pub fn report_progress(&mut self, id: TaskId, done_bytes: u64) -> bool {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        if !matches!(t.state, TaskState::Running) {
            return false;
        }
        // 进度不许后退：分片乱序到达时后退会让进度条来回跳，用户以为出错了
        t.done_bytes = t.done_bytes.max(done_bytes);
        if t.total_bytes > 0 {
            t.done_bytes = t.done_bytes.min(t.total_bytes);
        }
        true
    }

    /// 暂停。
    ///
    /// 进行中与等待中都可以暂停（界面上那两行都有暂停按钮）。已完成 / 已取消
    /// 不行——对终态还能操作，说明界面没按状态过滤。
    ///
    /// # Errors
    ///
    /// 任务不存在，或当前状态不允许暂停。
    pub fn pause(&mut self, id: TaskId) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        match t.state {
            TaskState::Running | TaskState::Waiting(_) | TaskState::Queued => {
                t.state = TaskState::Paused;
                self.pump();
                Ok(())
            }
            _ => Err(TransferError::InvalidTransition),
        }
    }

    /// 全部暂停。返回**实际**暂停的条数。
    ///
    /// 计数只算真的转成了已暂停的那些，而不是「筛出来打算暂停的个数」：界面会把
    /// 这个数字直接写成「已暂停 N 项」，按打算数报会在列表里只停了 2 条时说 3 条。
    ///
    /// 预筛状态只是避免对明显不该动的任务白调一次；真正的把关在
    /// [`TransferManager::pause`] 里（失败态会被它拒绝）。两处都判是刻意的：
    /// 把一条失败的任务改成「已暂停」会让它的重试按钮消失，用户再也没法让它继续，
    /// 所以这条不能只靠调用方筛对。
    pub fn pause_all(&mut self) -> usize {
        let ids: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.state,
                    TaskState::Running | TaskState::Waiting(_) | TaskState::Queued
                )
            })
            .map(|t| t.id)
            .collect();
        ids.into_iter().filter(|id| self.pause(*id).is_ok()).count()
    }

    /// 继续。
    ///
    /// **回到 `Queued` 而不是直接 `Running`**：并发额度可能已被别的任务占满，
    /// 直接置为 Running 会突破上限。随后 `pump` 会在有额度时提升它。
    /// 已有进度保留——暂停后从头再来会让用户不敢暂停。
    ///
    /// # Errors
    ///
    /// 任务不存在，或当前不是已暂停状态。
    pub fn resume(&mut self, id: TaskId) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        if !matches!(t.state, TaskState::Paused) {
            return Err(TransferError::InvalidTransition);
        }
        t.state = TaskState::Queued;
        self.pump();
        Ok(())
    }

    /// 报告被服务端限流，转入等待。
    ///
    /// **这是一次状态转移，不是失败**。等待会自动恢复（[`TransferManager::tick`]），
    /// 画成失败会让用户去点重试，而限流下重试只会把等待时间越点越长。
    ///
    /// 已有进度保留：限流跟数据完整性无关，没有理由作废已下好的部分。
    ///
    /// # Errors
    ///
    /// 任务不存在，或当前不是进行中。
    pub fn rate_limited(&mut self, id: TaskId, secs: u32) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        if !matches!(t.state, TaskState::Running) {
            return Err(TransferError::InvalidTransition);
        }
        t.state = TaskState::Waiting(WaitReason::RateLimited { secs });
        Ok(())
    }

    /// 时间推进 `secs` 秒：递减等待倒计时，到点的任务自动回到进行中。
    ///
    /// 取外部传入的流逝秒数而不是自己读时钟，是为了让这条规则能被确定性地测到。
    /// 读时钟的状态机只能靠 sleep 去测，那种测试既慢又会偶发失败。
    ///
    /// 返回自动恢复的任务数。
    pub fn tick(&mut self, secs: u32) -> usize {
        let mut resumed = 0;
        for t in &mut self.tasks {
            if let TaskState::Waiting(WaitReason::RateLimited { secs: left }) = t.state {
                let rest = left.saturating_sub(secs);
                if rest == 0 {
                    // 等待期间一直占着并发额度，所以这里可以直接回到 Running，
                    // 不必再排一次队（见 TaskState::holds_slot 的说明）
                    t.state = TaskState::Running;
                    resumed += 1;
                } else {
                    t.state = TaskState::Waiting(WaitReason::RateLimited { secs: rest });
                }
            }
        }
        resumed
    }

    /// 报告失败。
    ///
    /// 会按 [`FailCause::resume`] 决定要不要把进度清零：需要从头重传时就得清零，
    /// 否则界面会显示「已中断 71%」然后点重试从 71% 继续——而服务端那 71% 早就
    /// 过期了，只会再失败一次。
    ///
    /// # Errors
    ///
    /// 任务不存在，或任务已是终态（终态还能失败说明有人重复上报）。
    pub fn fail(&mut self, id: TaskId, cause: FailCause) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        if t.state.is_terminal() {
            return Err(TransferError::InvalidTransition);
        }
        if matches!(cause.resume(), Resume::FromStart) {
            t.done_bytes = 0;
        }
        t.state = TaskState::Failed(cause);
        self.pump();
        Ok(())
    }

    /// 重试一条失败的任务。
    ///
    /// 只有可重试的失败才行：对磁盘满 / 认证失效重试是必然再失败一次，界面不该
    /// 给出这个按钮，后端也要拦住。
    ///
    /// # Errors
    ///
    /// 任务不存在，或当前不是**可重试的**失败状态。
    pub fn retry(&mut self, id: TaskId) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        let TaskState::Failed(cause) = t.state else {
            return Err(TransferError::InvalidTransition);
        };
        if !cause.retryable() {
            return Err(TransferError::InvalidTransition);
        }
        // 进度在 fail() 里已按 resume 策略处理过，这里不要再动它：
        // 在两处各清一次，迟早出现「本该续传的也被清零」
        t.state = TaskState::Queued;
        self.pump();
        Ok(())
    }

    /// 取消。返回已落盘部分要怎么处理。
    ///
    /// 返回值必须被消费：永久缓存任务取消后要丢弃半成品，忽略它会在永久层留下
    /// 一份既不计入上限、又不会被淘汰、用户还看不见的垃圾。
    ///
    /// # Errors
    ///
    /// 任务不存在，或任务已是终态。
    pub fn cancel(&mut self, id: TaskId) -> Result<CancelCleanup, TransferError> {
        let t = self.find_mut(id)?;
        if t.state.is_terminal() {
            return Err(TransferError::InvalidTransition);
        }
        t.state = TaskState::Cancelled;
        let cleanup = match t.kind {
            TransferKind::Pin => CancelCleanup::DropPartial,
            TransferKind::Upload | TransferKind::Download => CancelCleanup::KeepPartial,
        };
        self.pump();
        Ok(cleanup)
    }

    /// 标记完成。
    ///
    /// 会把 `done_bytes` 补齐到 `total_bytes`：差几个字节会让界面显示
    /// 「已完成 · 99%」，用户没法判断到底成没成。
    ///
    /// # Errors
    ///
    /// 任务不存在，或任务已是终态。
    pub fn complete(&mut self, id: TaskId) -> Result<(), TransferError> {
        let t = self.find_mut(id)?;
        if t.state.is_terminal() {
            return Err(TransferError::InvalidTransition);
        }
        if t.total_bytes > 0 {
            t.done_bytes = t.total_bytes;
        }
        t.state = TaskState::Done;
        self.pump();
        Ok(())
    }

    /// 移除一条任务（界面上失败行那个 ✕「移除」）。
    ///
    /// 只允许移除不再进行的任务：把一条还在跑的任务从列表里删掉，它仍在后台占
    /// 带宽，而用户以为已经停了——要停就该走 [`TransferManager::cancel`]。
    ///
    /// # Errors
    ///
    /// 任务不存在，或任务仍在进行 / 排队 / 等待。
    pub fn remove(&mut self, id: TaskId) -> Result<(), TransferError> {
        let Some(pos) = self.tasks.iter().position(|t| t.id == id) else {
            return Err(TransferError::NoSuchTask);
        };
        let st = self.tasks[pos].state;
        if matches!(
            st,
            TaskState::Running | TaskState::Queued | TaskState::Waiting(_)
        ) {
            return Err(TransferError::InvalidTransition);
        }
        self.tasks.remove(pos);
        self.pump();
        Ok(())
    }

    /// 清除已完成与已取消的任务，返回清掉的条数。
    ///
    /// **不清失败的**：失败的任务还等着用户决定重试还是放弃，被「清除已完成」
    /// 顺手扫掉就再也找不回来了——而用户点那个按钮的意思只是「把已经办完的收起来」。
    pub fn clear_completed(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks
            .retain(|t| !matches!(t.state, TaskState::Done | TaskState::Cancelled));
        before - self.tasks.len()
    }

    /// 汇总计数。
    #[must_use]
    pub fn summary(&self) -> TransferSummary {
        let mut s = TransferSummary::default();
        for t in &self.tasks {
            match t.state {
                TaskState::Running => s.running += 1,
                TaskState::Queued => s.queued += 1,
                TaskState::Waiting(_) => s.waiting += 1,
                TaskState::Paused => s.paused += 1,
                TaskState::Failed(_) => s.failed += 1,
                TaskState::Done => s.done += 1,
                TaskState::Cancelled => s.cancelled += 1,
            }
        }
        s
    }

    /// 同一个 `(位置, 条目, 类型)` 是否已有未结束的任务。
    ///
    /// 给调用方去重用：用户在列表里连点两下「永久缓存」，不去重就会起两条任务
    /// 抢同一个文件，进度条互相覆盖、流量翻倍。
    #[must_use]
    pub fn find_active(
        &self,
        kind: TransferKind,
        place: &str,
        entry_id: &str,
    ) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|t| {
                t.kind == kind
                    && t.place == place
                    && t.entry_id == entry_id
                    && !t.state.is_terminal()
                    && !matches!(t.state, TaskState::Failed(_))
            })
            .map(|t| t.id)
    }

    /// 按位置分组的活动任务数，供位置列表上显示角标。
    #[must_use]
    pub fn active_by_place(&self) -> HashMap<String, u32> {
        let mut m: HashMap<String, u32> = HashMap::new();
        for t in &self.tasks {
            if t.state.holds_slot() || matches!(t.state, TaskState::Queued) {
                *m.entry(t.place.clone()).or_insert(0) += 1;
            }
        }
        m
    }

    fn find_mut(&mut self, id: TaskId) -> Result<&mut Task, TransferError> {
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(TransferError::NoSuchTask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr(max: usize) -> TransferManager {
        TransferManager::new(max)
    }

    fn add(m: &mut TransferManager, kind: TransferKind, name: &str, total: u64) -> TaskId {
        m.enqueue(kind, "tg", format!("/chat/{name}"), name, total)
    }

    /// 超过并发上限的任务必须排队，且排队不等于 0%。
    ///
    /// 不这样会怎样：一次性起十几条传输会撞上服务端风控，而风控的后果是账号级
    /// 别的。另外排队若显示成 0% 进度条，用户会以为卡死了而反复点。
    #[test]
    fn over_limit_tasks_queue_instead_of_running() {
        let mut m = mgr(2);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        let b = add(&mut m, TransferKind::Download, "b", 1000);
        let c = add(&mut m, TransferKind::Download, "c", 1000);

        assert_eq!(m.get(a).expect("a").state, TaskState::Running);
        assert_eq!(m.get(b).expect("b").state, TaskState::Running);
        assert_eq!(m.get(c).expect("c").state, TaskState::Queued, "第三条必须排队");
        assert_eq!(m.summary().running, 2, "并发不能超过上限");

        // 排队中的任务没有百分比可显示的问题：它的进度确实是 0 字节，
        // 但界面靠 state 判断显示「排队中」，这里确认状态是可区分的
        assert_ne!(
            m.get(c).expect("c").state,
            TaskState::Running,
            "排队与进行中必须是两个可区分的状态，否则界面只能画 0%"
        );

        // 一条完成后，排队的那条要顶上
        m.complete(a).expect("完成 a");
        assert_eq!(m.get(c).expect("c").state, TaskState::Running, "完成后应补跑排队任务");
    }

    /// 上限为 0 要按 1 处理。
    ///
    /// 不这样会怎样：所有任务永远排队，界面上表现为「点了下载但什么也不发生」，
    /// 而这种配置错误没有任何提示。
    #[test]
    fn zero_limit_falls_back_to_one() {
        let mut m = mgr(0);
        assert_eq!(m.max_running(), 1);
        let a = add(&mut m, TransferKind::Download, "a", 10);
        assert_eq!(m.get(a).expect("a").state, TaskState::Running, "至少要跑得起来一条");
    }

    /// 限流必须是「等待中」，不能是失败，而且不给重试按钮。
    ///
    /// 不这样会怎样：用户看到「失败」就去点重试，而限流期间重试只会把等待时间
    /// 越点越长——一个越修越坏的循环。
    #[test]
    fn rate_limited_is_waiting_not_failed() {
        let mut m = mgr(2);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.report_progress(a, 400);
        m.rate_limited(a, 37).expect("限流");

        let t = m.get(a).expect("a");
        assert_eq!(t.state, TaskState::Waiting(WaitReason::RateLimited { secs: 37 }));
        assert!(!matches!(t.state, TaskState::Failed(_)), "限流不是失败");
        assert!(!t.can_retry(), "等待中不该给重试按钮");
        assert_eq!(t.done_bytes, 400, "限流与数据完整性无关，进度不该作废");
        assert_eq!(m.summary().waiting, 1);
        assert_eq!(m.summary().failed, 0, "等待绝不能计进失败数");
    }

    /// 等待倒计时走完要自动回到进行中。
    ///
    /// 不这样会怎样：任务永远停在「等待 0s」，用户只能自己去点——而这本该是
    /// 自动恢复的。
    #[test]
    fn waiting_resumes_automatically_after_countdown() {
        let mut m = mgr(2);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.rate_limited(a, 10).expect("限流");

        assert_eq!(m.tick(4), 0, "还没到点不该恢复");
        assert_eq!(
            m.get(a).expect("a").state,
            TaskState::Waiting(WaitReason::RateLimited { secs: 6 }),
            "倒计时要递减，界面才能显示剩余秒数"
        );
        assert_eq!(m.tick(6), 1, "到点应自动恢复");
        assert_eq!(m.get(a).expect("a").state, TaskState::Running);
    }

    /// 等待中的任务仍占并发额度。
    ///
    /// 不这样会怎样：一条任务被限流就放另一条进来，两条一起挨同样的限流，
    /// 还让服务端看到更多请求——把风控问题放大而不是缓解。
    ///
    /// 断言必须**在限流之后再触发一次 pump**（这里用「新任务入队」，那正是产品里
    /// 最常见的触发点）。只检查「限流后 b 仍在排队」是抓不到缺陷的：`rate_limited`
    /// 自己不调 pump，额度算错要等下一次 pump 才暴露。这一点是实测出来的——
    /// 把 `holds_slot` 改成只认 `Running`，不补这一步的话变异能存活。
    #[test]
    fn waiting_task_still_holds_its_slot() {
        let mut m = mgr(1);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        let b = add(&mut m, TransferKind::Download, "b", 1000);
        assert_eq!(m.get(b).expect("b").state, TaskState::Queued);

        m.rate_limited(a, 30).expect("限流");
        // 新任务入队会触发 pump：此刻若把等待中的 a 不算额度，b 就会被提升
        let c = add(&mut m, TransferKind::Download, "c", 1000);
        assert_eq!(
            m.get(b).expect("b").state,
            TaskState::Queued,
            "限流期间不该放新任务进来一起挨打"
        );
        assert_eq!(m.get(c).expect("c").state, TaskState::Queued);
        assert_eq!(m.summary().running, 0, "上限为 1 且那一条正在等待，不该有任务在跑");
        assert_eq!(m.summary().waiting, 1);
    }

    /// 暂停后继续要接着原进度，且要重新排队而不是直接抢额度。
    ///
    /// 不这样会怎样：暂停等于从头再来，用户就不敢暂停了；而直接置为 Running
    /// 会突破并发上限，正好撞上风控。
    #[test]
    fn resume_keeps_progress_and_respects_limit() {
        let mut m = mgr(1);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.report_progress(a, 700);
        m.pause(a).expect("暂停");
        assert_eq!(m.get(a).expect("a").state, TaskState::Paused);

        // 暂停让出额度，另一条顶上
        let b = add(&mut m, TransferKind::Download, "b", 1000);
        assert_eq!(m.get(b).expect("b").state, TaskState::Running);

        m.resume(a).expect("继续");
        let t = m.get(a).expect("a");
        assert_eq!(t.done_bytes, 700, "继续必须接着原进度");
        assert_eq!(t.state, TaskState::Queued, "额度被占满时只能排队，不能突破上限");
        assert_eq!(m.summary().running, 1);
    }

    /// 分片过期这类失败要清零进度；网络失败要保留。
    ///
    /// 不这样会怎样：界面显示「已中断 71%」，用户点重试、从 71% 接着传——
    /// 而服务端那 71% 早就过期了，只会再失败一次。反过来，把能续传的也清零
    /// 则是白白重下一遍几 GB。
    #[test]
    fn expired_resets_progress_but_network_keeps_it() {
        let mut m = mgr(4);

        let a = add(&mut m, TransferKind::Upload, "a", 1000);
        m.report_progress(a, 710);
        m.fail(a, FailCause::Expired).expect("过期失败");
        assert_eq!(
            m.get(a).expect("a").done_bytes,
            0,
            "必须从头开始时进度要清零，否则重试按钮是在骗人"
        );
        assert_eq!(FailCause::Expired.resume(), Resume::FromStart);
        assert!(m.get(a).expect("a").can_retry());

        let b = add(&mut m, TransferKind::Download, "b", 1000);
        m.report_progress(b, 710);
        m.fail(b, FailCause::Network).expect("网络失败");
        assert_eq!(m.get(b).expect("b").done_bytes, 710, "能续传的不该清零");
        assert_eq!(FailCause::Network.resume(), Resume::FromProgress);

        // 重试不能再清一次进度：两处各清一次会让本该续传的也被清零
        m.retry(b).expect("重试 b");
        assert_eq!(m.get(b).expect("b").done_bytes, 710);
    }

    /// 磁盘满与认证失效不可重试。
    ///
    /// 不这样会怎样：给出一个点了必然再失败的重试按钮。磁盘满要用户先腾空间，
    /// 认证失效要重新登录——两者都不是重试能解决的，而界面无限转圈会让用户
    /// 完全不知道该做什么。
    #[test]
    fn unretryable_failures_reject_retry() {
        let mut m = mgr(4);
        for cause in [FailCause::DiskFull, FailCause::Unauthorized, FailCause::Unsupported] {
            let id = add(&mut m, TransferKind::Download, "x", 100);
            m.fail(id, cause).expect("失败");
            assert!(!cause.retryable(), "{cause:?} 不该被判为可重试");
            assert!(!m.get(id).expect("任务").can_retry(), "界面不该给重试按钮");
            assert_eq!(
                m.retry(id),
                Err(TransferError::InvalidTransition),
                "后端也要拦住 {cause:?} 的重试，不能只靠界面过滤"
            );
        }
    }

    /// 取消永久缓存任务后不能留半成品。
    ///
    /// 不这样会怎样：那些块留在永久层里，既不计入上限、也不会被 LRU 淘汰、
    /// 用户在管理列表里还看不到（标记已随取消删掉）——一份永远不会被清理的垃圾。
    #[test]
    fn cancelling_pin_task_drops_partial_data() {
        let mut m = mgr(4);
        let pin = add(&mut m, TransferKind::Pin, "movie", 5000);
        m.report_progress(pin, 2000);
        assert_eq!(
            m.cancel(pin).expect("取消"),
            CancelCleanup::DropPartial,
            "永久缓存取消后必须丢弃半成品"
        );

        // 普通下载相反：已缓存的临时块留着能省流量，且它本就会被 LRU 回收
        let dl = add(&mut m, TransferKind::Download, "clip", 5000);
        m.report_progress(dl, 2000);
        assert_eq!(m.cancel(dl).expect("取消"), CancelCleanup::KeepPartial);
    }

    /// 永久缓存的角标只在完成后出现。
    ///
    /// 不这样会怎样：钉住的那一刻就显示 📌，用户以为已经能离线打开了——
    /// 而此时文件还在下载，断网就打不开。
    #[test]
    fn pin_badge_only_after_completion() {
        let mut m = mgr(4);
        let pin = add(&mut m, TransferKind::Pin, "doc", 100);
        assert!(!m.get(pin).expect("任务").pin_badge_visible(), "下载中不能显示角标");
        m.report_progress(pin, 99);
        assert!(!m.get(pin).expect("任务").pin_badge_visible(), "99% 也还不能显示");
        m.complete(pin).expect("完成");
        assert!(m.get(pin).expect("任务").pin_badge_visible(), "完成后才显示");

        // 普通下载完成也不该长出 📌
        let dl = add(&mut m, TransferKind::Download, "d", 100);
        m.complete(dl).expect("完成");
        assert!(!m.get(dl).expect("任务").pin_badge_visible());
    }

    /// 终态不接受任何再次操作。
    ///
    /// 不这样会怎样：迟到的回调能让一条已取消的任务复活并继续占带宽，
    /// 或让一条已完成的任务变回进行中——用户看到的是一个自己会动的列表。
    #[test]
    fn terminal_states_reject_further_operations() {
        let mut m = mgr(4);
        let done = add(&mut m, TransferKind::Download, "a", 100);
        m.complete(done).expect("完成");
        assert_eq!(m.pause(done), Err(TransferError::InvalidTransition));
        assert_eq!(m.retry(done), Err(TransferError::InvalidTransition));
        assert_eq!(m.cancel(done), Err(TransferError::InvalidTransition));
        assert_eq!(m.fail(done, FailCause::Network), Err(TransferError::InvalidTransition));
        assert!(!m.report_progress(done, 50), "已完成不该再收进度");
        assert_eq!(m.get(done).expect("a").state, TaskState::Done, "状态不能被改掉");

        let cancelled = add(&mut m, TransferKind::Download, "b", 100);
        m.cancel(cancelled).expect("取消");
        assert!(!m.report_progress(cancelled, 50), "已取消的任务不能靠进度回调复活");
        assert_eq!(m.get(cancelled).expect("b").state, TaskState::Cancelled);
    }

    /// 已暂停的任务不接受迟到的进度回调。
    ///
    /// 不这样会怎样：用户点了暂停，进度条却还在往前走——他会以为暂停没生效。
    #[test]
    fn paused_task_ignores_late_progress() {
        let mut m = mgr(4);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.report_progress(a, 300);
        m.pause(a).expect("暂停");
        assert!(!m.report_progress(a, 500), "暂停后应拒收进度");
        assert_eq!(m.get(a).expect("a").done_bytes, 300);
    }

    /// 进度不许后退，也不许超过总量。
    ///
    /// 不这样会怎样：分片乱序到达时进度条来回跳，或显示 103%，两者都让用户
    /// 以为程序出错了。
    #[test]
    fn progress_is_monotonic_and_capped() {
        let mut m = mgr(4);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.report_progress(a, 600);
        m.report_progress(a, 400);
        assert_eq!(m.get(a).expect("a").done_bytes, 600, "进度不能后退");
        m.report_progress(a, 99_999);
        assert_eq!(m.get(a).expect("a").done_bytes, 1000, "不能超过总量");
        assert_eq!(m.get(a).expect("a").percent(), Some(100));
    }

    /// 总量未知时不能把进度报成 0%。
    ///
    /// 不这样会怎样：「不知道多大」和「还没开始」被画成同一个样子，
    /// 而它们对用户的含义完全不同。
    #[test]
    fn unknown_total_has_no_percent() {
        let mut m = mgr(4);
        let a = add(&mut m, TransferKind::Upload, "a", 0);
        assert_eq!(m.get(a).expect("a").percent(), None, "总量未知不能返回 0");
        m.report_progress(a, 12345);
        assert_eq!(m.get(a).expect("a").percent(), None);
        assert_eq!(m.get(a).expect("a").done_bytes, 12345, "字节数仍要如实记");
    }

    /// 完成时要把进度补齐。
    ///
    /// 不这样会怎样：界面显示「已完成 · 99%」，用户没法判断到底成没成。
    #[test]
    fn completion_fills_progress() {
        let mut m = mgr(4);
        let a = add(&mut m, TransferKind::Download, "a", 1000);
        m.report_progress(a, 990);
        m.complete(a).expect("完成");
        assert_eq!(m.get(a).expect("a").percent(), Some(100));
    }

    /// 「全部暂停」不能动失败与终态的任务，返回的条数也要是**实际**暂停数。
    ///
    /// 不这样会怎样：一条失败的任务被改成「已暂停」，它的重试按钮就消失了，
    /// 用户再也没法让它继续。而返回值若按「打算暂停的个数」报，界面会在只停了
    /// 2 条时说「已暂停 3 项」。
    ///
    /// 真正把关的是 `pause` 本身会拒绝失败态，`pause_all` 的预筛只是少白调几次；
    /// 所以这里既断言状态、也断言返回值——只断言状态的话，把预筛条件放宽到
    /// 「非终态」这种改动抓不出来（`pause` 仍会拒绝，状态不变）。
    #[test]
    fn pause_all_leaves_failed_and_terminal_alone() {
        let mut m = mgr(3);
        let run = add(&mut m, TransferKind::Download, "r", 100);
        let bad = add(&mut m, TransferKind::Download, "f", 100);
        let fin = add(&mut m, TransferKind::Download, "d", 100);
        m.fail(bad, FailCause::Network).expect("失败");
        m.complete(fin).expect("完成");
        let queued = add(&mut m, TransferKind::Download, "q", 100);

        assert_eq!(m.pause_all(), 2, "只有 r 与 q 能被暂停，返回值必须是实际数");
        assert_eq!(m.get(run).expect("r").state, TaskState::Paused);
        assert_eq!(m.get(queued).expect("q").state, TaskState::Paused, "排队的也该暂停");
        assert_eq!(
            m.get(bad).expect("f").state,
            TaskState::Failed(FailCause::Network),
            "失败的任务不能被改成已暂停，否则重试按钮就没了"
        );
        assert_eq!(m.get(fin).expect("d").state, TaskState::Done);
        assert_eq!(m.pause_all(), 0, "已经全停了，再点一次应报 0 而不是重复计数");
    }

    /// `pause` 本身必须拒绝失败态，不能只靠 `pause_all` 预筛。
    ///
    /// 不这样会怎样：一条失败的任务被改成「已暂停」，它的重试按钮就消失了，
    /// 用户再也没法让它继续，只能取消重来。
    ///
    /// 这条要单独测：`pause_all` 会先筛掉失败态，所以那个测试走不到 `pause` 的
    /// 守卫；而界面上失败行旁边就有别的按钮，误传一次 pause 完全可能发生。
    #[test]
    fn pause_refuses_failed_task() {
        let mut m = mgr(3);
        let bad = add(&mut m, TransferKind::Download, "f", 100);
        m.fail(bad, FailCause::Network).expect("失败");

        assert_eq!(
            m.pause(bad),
            Err(TransferError::InvalidTransition),
            "失败态不能被暂停，否则重试按钮会消失"
        );
        assert_eq!(
            m.get(bad).expect("f").state,
            TaskState::Failed(FailCause::Network),
            "状态必须原样保留"
        );
        assert!(m.get(bad).expect("f").can_retry(), "重试按钮要还在");
    }

    /// 「清除已完成」不能顺手清掉失败的。
    ///
    /// 不这样会怎样：失败的任务还等着用户决定重试还是放弃，被扫掉就再也找不
    /// 回来了——而用户点那个按钮的意思只是「把已经办完的收起来」。
    #[test]
    fn clear_completed_keeps_failed() {
        let mut m = mgr(4);
        let done = add(&mut m, TransferKind::Download, "d", 100);
        let cancelled = add(&mut m, TransferKind::Download, "c", 100);
        let failed = add(&mut m, TransferKind::Download, "f", 100);
        let running = add(&mut m, TransferKind::Download, "r", 100);
        m.complete(done).expect("完成");
        m.cancel(cancelled).expect("取消");
        m.fail(failed, FailCause::Network).expect("失败");

        assert_eq!(m.clear_completed(), 2, "只清已完成与已取消");
        assert!(m.get(done).is_none());
        assert!(m.get(cancelled).is_none());
        assert!(m.get(failed).is_some(), "失败的必须留着，它还等用户决定");
        assert!(m.get(running).is_some());
    }

    /// 还在跑的任务不能被 remove 悄悄从列表里抹掉。
    ///
    /// 不这样会怎样：任务仍在后台占带宽，而用户以为已经停了——要停就该走
    /// cancel，那才会真的通知驱动收手。
    #[test]
    fn remove_refuses_active_tasks() {
        let mut m = mgr(2);
        let run = add(&mut m, TransferKind::Download, "r", 100);
        assert_eq!(m.remove(run), Err(TransferError::InvalidTransition));

        m.rate_limited(run, 5).expect("限流");
        assert_eq!(m.remove(run), Err(TransferError::InvalidTransition), "等待中也不行");

        // 失败的可以移除（界面上失败行那个 ✕「移除」）
        let bad = add(&mut m, TransferKind::Download, "b", 100);
        m.fail(bad, FailCause::DiskFull).expect("失败");
        assert!(m.remove(bad).is_ok(), "失败的应当可以移除");
        assert_eq!(m.remove(TaskId(9999)), Err(TransferError::NoSuchTask));
    }

    /// 同一个文件不能起两条并行任务。
    ///
    /// 不这样会怎样：用户连点两下「永久缓存」，两条任务抢同一个文件，
    /// 进度条互相覆盖、流量翻倍。
    #[test]
    fn duplicate_active_task_is_detectable() {
        let mut m = mgr(4);
        let a = m.enqueue(TransferKind::Pin, "tg", "/c/1", "x", 100);
        assert_eq!(m.find_active(TransferKind::Pin, "tg", "/c/1"), Some(a));
        // 类型不同不算重复：同一个文件可以既在下载又在转永久
        assert_eq!(m.find_active(TransferKind::Download, "tg", "/c/1"), None);
        // 位置不同也不算：同名条目可能来自不同位置
        assert_eq!(m.find_active(TransferKind::Pin, "nas", "/c/1"), None);

        m.complete(a).expect("完成");
        assert_eq!(
            m.find_active(TransferKind::Pin, "tg", "/c/1"),
            None,
            "已结束的不该挡住新任务"
        );
    }

    /// 错误映射：限流不该变成失败，会话失效不该变成可重试。
    ///
    /// 不这样会怎样：`Unauthorized` 若被映射成可重试，界面会无限转圈重试一个
    /// 永远不会成功的请求，而真正要做的是弹重新登录。
    #[test]
    fn error_mapping_keeps_retryability_honest() {
        assert_eq!(FailCause::from(&crate::Error::Network(String::from("x"))), FailCause::Network);
        assert_eq!(FailCause::from(&crate::Error::Unauthorized), FailCause::Unauthorized);
        assert!(!FailCause::from(&crate::Error::Unauthorized).retryable());
        assert_eq!(FailCause::from(&crate::Error::Forbidden), FailCause::Unauthorized);
        assert_eq!(
            FailCause::from(&crate::Error::Unsupported("x")),
            FailCause::Unsupported
        );
        assert!(!FailCause::from(&crate::Error::Unsupported("x")).retryable());

        // 磁盘满必须与普通 I/O 错误分开：一个不给重试按钮，一个给
        let full = crate::Error::Io(std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert_eq!(FailCause::from(&full), FailCause::DiskFull);
        assert!(!FailCause::from(&full).retryable());
        let other = crate::Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(FailCause::from(&other), FailCause::Other);
        assert!(FailCause::from(&other).retryable());
    }

    /// 汇总计数里等待与失败必须分开。
    ///
    /// 不这样会怎样：状态栏把限流等待计进失败数，用户看到「2 失败」就去点重试，
    /// 而其中一条本来会自动恢复。
    #[test]
    fn summary_separates_waiting_from_failed() {
        let mut m = mgr(3);
        let w = add(&mut m, TransferKind::Download, "w", 100);
        let f = add(&mut m, TransferKind::Download, "f", 100);
        add(&mut m, TransferKind::Download, "r", 100);
        let q = add(&mut m, TransferKind::Download, "q", 100);
        m.rate_limited(w, 20).expect("限流");
        m.fail(f, FailCause::Network).expect("失败");

        let s = m.summary();
        assert_eq!(s.waiting, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.running, 2, "失败让出的额度要被排队任务顶上");
        assert_eq!(s.queued, 0);
        assert_eq!(m.get(q).expect("q").state, TaskState::Running);
    }

    /// 按位置分组的活动计数只算未结束的任务。
    #[test]
    fn active_by_place_counts_only_live_tasks() {
        let mut m = mgr(4);
        m.enqueue(TransferKind::Download, "tg", "/a", "a", 10);
        m.enqueue(TransferKind::Download, "tg", "/b", "b", 10);
        let nas = m.enqueue(TransferKind::Download, "nas", "/c", "c", 10);
        let by = m.active_by_place();
        assert_eq!(by.get("tg").copied(), Some(2));
        assert_eq!(by.get("nas").copied(), Some(1));

        m.complete(nas).expect("完成");
        assert_eq!(
            m.active_by_place().get("nas").copied(),
            None,
            "已完成不该还在活动计数里"
        );
    }
}
