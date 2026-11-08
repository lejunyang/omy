//! Telegram 高级查询的共享契约与纯逻辑。GUI/CLI 必须复用同一棵条件树求值。
use std::collections::BTreeSet;

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

use super::store::TelegramId;
use crate::{Error, Result};

pub const MAX_GROUP_DEPTH: usize = 2;
pub const MAX_CONDITIONS: usize = 100;
pub const MAX_PAGE_SCAN: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    Gif,
    Audio,
    Voice,
    File,
    Link,
}

impl MediaKind {
    pub fn filter(self) -> grammers_client::tl::enums::MessagesFilter {
        use grammers_client::tl::enums::MessagesFilter as F;
        match self {
            Self::Photo => F::InputMessagesFilterPhotos,
            Self::Video => F::InputMessagesFilterVideo,
            Self::Gif => F::InputMessagesFilterGif,
            Self::Audio => F::InputMessagesFilterMusic,
            Self::Voice => F::InputMessagesFilterVoice,
            Self::File => F::InputMessagesFilterDocument,
            Self::Link => F::InputMessagesFilterUrl,
        }
    }
    pub const fn tab(self) -> &'static str {
        match self {
            Self::Photo | Self::Video => "media",
            Self::Gif => "gif",
            Self::Audio | Self::Voice => "audio",
            Self::Link => "link",
            Self::File => "file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    All,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Media {
        types: Vec<MediaKind>,
        negate: bool,
    },
    Ext {
        exts: Vec<String>,
        negate: bool,
    },
    Size {
        min: Option<u64>,
        max: Option<u64>,
        negate: bool,
    },
    Text {
        text: String,
        negate: bool,
    },
    Date {
        from: Option<i64>,
        to: Option<i64>,
        negate: bool,
    },
    Group {
        r#match: Match,
        conditions: Vec<Condition>,
        negate: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub r#match: Match,
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub message: i32,
    pub date: i64,
    pub text: String,
    pub media: Option<MediaKind>,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub thumb_token: Option<String>,
    #[serde(skip)]
    pub thumb: Option<Vec<u8>>,
    // 一条视频说明也可能含链接，不能因主媒体是视频就漏掉 link 条件。
    #[serde(skip)]
    pub has_link: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub cursor: Option<String>,
    pub scanned: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selection {
    Ids { ids: Vec<String> },
    All { except: Vec<String> },
}

impl Selection {
    pub fn validate(&self, chat: i64) -> Result<()> {
        let ids = match self {
            Self::Ids { ids } => ids,
            Self::All { except } => except,
        };
        if matches!(self, Self::Ids { .. }) && ids.is_empty() {
            return Err(invalid());
        }
        for id in ids {
            let parsed = TelegramId::decode(id)?;
            if parsed.chat != chat || parsed.message <= 0 || parsed.encode() != *id {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn includes(&self, id: &str) -> bool {
        match self {
            Self::Ids { ids } => ids.iter().any(|v| v == id),
            Self::All { except } => !except.iter().any(|v| v == id),
        }
    }
    pub fn message_ids(&self) -> Vec<i32> {
        match self {
            Self::Ids { ids } => ids
                .iter()
                .filter_map(|id| TelegramId::decode(id).ok().map(|v| v.message))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .rev()
                .collect(),
            Self::All { .. } => Vec::new(),
        }
    }
}

fn invalid() -> Error {
    Error::Protocol(String::from("invalid telegram advanced query"))
}
fn bounds<T: Ord>(min: Option<T>, max: Option<T>) -> bool {
    (min.is_some() || max.is_some()) && !min.zip(max).is_some_and(|(a, b)| a > b)
}

impl Query {
    pub fn validate(&self) -> Result<()> {
        fn nodes(list: &[Condition], depth: usize, count: &mut usize) -> bool {
            if list.is_empty() || depth > MAX_GROUP_DEPTH {
                return false;
            }
            for node in list {
                *count += 1;
                if *count > MAX_CONDITIONS {
                    return false;
                }
                let ok = match node {
                    Condition::Group { conditions, .. } => nodes(conditions, depth + 1, count),
                    Condition::Media { types, .. } => !types.is_empty() && types.len() <= 7,
                    Condition::Ext { exts, .. } => {
                        !exts.is_empty()
                            && exts.len() <= 100
                            && exts.iter().all(|s| {
                                s.len() <= 64
                                    && !s.is_empty()
                                    && s == &s.to_lowercase()
                                    && !s
                                        .chars()
                                        .any(|c| c.is_whitespace() || matches!(c, '.' | '/' | '\\'))
                            })
                    }
                    Condition::Size { min, max, .. } => bounds(*min, *max),
                    Condition::Text { text, .. } => !text.trim().is_empty() && text.len() <= 4096,
                    Condition::Date { from, to, .. } => bounds(*from, *to),
                };
                if !ok {
                    return false;
                }
            }
            true
        }
        if nodes(&self.conditions, 0, &mut 0) {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    pub fn matches(&self, hit: &SearchHit) -> bool {
        evaluate(self.r#match, &self.conditions, hit)
    }

    // 只能下推整棵树必需的根条件；OR 或子组条件被下推会漏掉其他分支的命中。
    pub fn plan(&self) -> SearchPlan {
        let mut plan = SearchPlan::default();
        if self.r#match != Match::All && self.conditions.len() != 1 {
            return plan;
        }
        for node in &self.conditions {
            match node {
                Condition::Media {
                    types,
                    negate: false,
                } if types.len() == 1 && plan.media.is_none() => {
                    plan.media = types.first().copied()
                }
                Condition::Text {
                    text,
                    negate: false,
                } if plan.text.is_none() => plan.text = Some(text.clone()),
                Condition::Date {
                    from,
                    to,
                    negate: false,
                } => {
                    if let Some(v) = from {
                        plan.from = Some(plan.from.map_or(*v, |old| old.max(*v)));
                    }
                    if let Some(v) = to {
                        plan.to = Some(plan.to.map_or(*v, |old| old.min(*v)));
                    }
                }
                _ => {}
            }
        }
        plan
    }
    fn fingerprint(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        Ok(format!("{:x}", Blake2s256::digest(bytes)))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SearchPlan {
    pub media: Option<MediaKind>,
    pub text: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

fn evaluate(mode: Match, nodes: &[Condition], hit: &SearchHit) -> bool {
    let one = |node: &Condition| {
        let (value, negate) = match node {
            Condition::Group {
                r#match,
                conditions,
                negate,
            } => (evaluate(*r#match, conditions, hit), *negate),
            Condition::Media { types, negate } => (
                types.iter().any(|kind| {
                    if *kind == MediaKind::Link {
                        hit.has_link
                    } else {
                        hit.media == Some(*kind)
                    }
                }),
                *negate,
            ),
            Condition::Ext { exts, negate } => (
                hit.file_name
                    .as_ref()
                    .and_then(|name| name.rsplit_once('.'))
                    .is_some_and(|(_, ext)| exts.contains(&ext.to_lowercase())),
                *negate,
            ),
            Condition::Size { min, max, negate } => (
                hit.file_size.is_some_and(|size| {
                    min.is_none_or(|v| size >= v) && max.is_none_or(|v| size <= v)
                }),
                *negate,
            ),
            Condition::Text { text, negate } => (
                hit.text.to_lowercase().contains(&text.to_lowercase()),
                *negate,
            ),
            Condition::Date { from, to, negate } => (
                from.is_none_or(|v| hit.date >= v) && to.is_none_or(|v| hit.date <= v),
                *negate,
            ),
        };
        value != negate
    };
    match mode {
        Match::All => nodes.iter().all(one),
        Match::Any => nodes.iter().any(one),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    chat: i64,
    fingerprint: String,
    pub before: i32,
    pub scanned: u64,
}
impl Cursor {
    pub fn start(chat: i64, query: &Query, cursor: Option<&str>) -> Result<Self> {
        let fingerprint = query.fingerprint()?;
        match cursor {
            None => Ok(Self {
                chat,
                fingerprint,
                before: 0,
                scanned: 0,
            }),
            Some(raw) => {
                if raw.len() > 512 {
                    return Err(invalid());
                }
                let cur: Self = serde_json::from_str(raw).map_err(|_| invalid())?;
                if cur.chat != chat || cur.fingerprint != fingerprint || cur.before <= 0 {
                    return Err(invalid());
                }
                Ok(cur)
            }
        }
    }
    pub fn advance(&mut self, message: i32) -> Result<()> {
        if message <= 0 || (self.before > 0 && message >= self.before) {
            return Err(invalid());
        }
        self.before = message;
        self.scanned = self.scanned.saturating_add(1);
        Ok(())
    }
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|_| invalid())
    }
}

/// RPC 和测试共用分页收集器；只推进实际读过的消息，不以命中条数充当扫描数。
pub struct PageScan<'a> {
    query: &'a Query,
    cursor: Cursor,
    limit: usize,
    hits: Vec<SearchHit>,
    exhausted: bool,
}
impl<'a> PageScan<'a> {
    pub fn new(query: &'a Query, cursor: Cursor, limit: usize) -> Self {
        Self {
            query,
            cursor,
            limit,
            hits: Vec::new(),
            exhausted: false,
        }
    }
    pub fn push(&mut self, hit: SearchHit) -> Result<bool> {
        self.cursor.advance(hit.message)?;
        if self.query.plan().from.is_some_and(|from| hit.date < from) {
            self.exhausted = true;
            return Ok(false);
        }
        if self.query.matches(&hit) {
            self.hits.push(hit);
        }
        Ok(self.hits.len() < self.limit)
    }
    pub fn finish(self, exhausted: bool) -> Result<SearchPage> {
        Ok(SearchPage {
            hits: self.hits,
            scanned: self.cursor.scanned,
            cursor: if exhausted || self.exhausted {
                None
            } else {
                Some(self.cursor.encode()?)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hit() -> SearchHit {
        SearchHit {
            id: String::from("tg:1:7"),
            message: 7,
            date: 100,
            text: String::from("Weekly REPORT"),
            media: None,
            file_name: None,
            file_size: None,
            thumb_token: None,
            thumb: None,
            has_link: false,
        }
    }
    fn query(nodes: Vec<Condition>) -> Query {
        Query {
            r#match: Match::All,
            conditions: nodes,
        }
    }
    #[test]
    fn absent_file_and_recursive_negation() {
        let q = query(vec![Condition::Group {
            r#match: Match::Any,
            negate: true,
            conditions: vec![
                Condition::Ext {
                    exts: vec![String::from("mkv")],
                    negate: false,
                },
                Condition::Size {
                    min: Some(500),
                    max: None,
                    negate: false,
                },
            ],
        }]);
        assert!(q.matches(&hit()), "无文件两条都不满足，整组取反必须命中");
        let mut file = hit();
        file.file_name = Some(String::from("x.MKV"));
        file.file_size = Some(499);
        assert!(
            !q.matches(&file),
            "扩展名不区分大小写，任一满足后组取反必须排除"
        );
        assert!(
            query(vec![Condition::Media {
                types: vec![MediaKind::Video],
                negate: true
            }])
            .matches(&hit()),
            "纯文本也须满足不是视频"
        );
    }
    #[test]
    fn bounds_include_both_ends_and_text_is_case_insensitive() {
        let q = query(vec![
            Condition::Date {
                from: Some(100),
                to: Some(200),
                negate: false,
            },
            Condition::Text {
                text: String::from("report"),
                negate: false,
            },
        ]);
        for date in [100, 200] {
            let mut h = hit();
            h.date = date;
            assert!(q.matches(&h), "闭区间边界不能丢且文字须忽略大小写");
        }
        for date in [99, 201] {
            let mut h = hit();
            h.date = date;
            assert!(!q.matches(&h), "区间外必须排除");
        }
        let mut h = hit();
        h.file_size = Some(500);
        assert!(
            query(vec![Condition::Size {
                min: Some(500),
                max: Some(500),
                negate: false
            }])
            .matches(&h),
            "相同大小上下限必须包含边界"
        );
    }
    #[test]
    fn validate_and_pushdown_do_not_widen_or_brake_or() {
        for q in [
            query(vec![]),
            query(vec![Condition::Group {
                r#match: Match::All,
                negate: false,
                conditions: vec![],
            }]),
            query(vec![Condition::Size {
                min: Some(2),
                max: Some(1),
                negate: false,
            }]),
            query(vec![Condition::Date {
                from: None,
                to: None,
                negate: true,
            }]),
            query(vec![Condition::Ext {
                exts: vec![String::from(".mkv")],
                negate: false,
            }]),
        ] {
            assert!(q.validate().is_err(), "畸形条件不能变成不限范围查询");
        }
        let mut q = query(vec![
            Condition::Media {
                types: vec![MediaKind::Video],
                negate: false,
            },
            Condition::Text {
                text: String::from("word"),
                negate: false,
            },
        ]);
        assert_eq!(
            q.plan().media,
            Some(MediaKind::Video),
            "AND 必需的媒体可以下推"
        );
        q.r#match = Match::Any;
        assert_eq!(
            q.plan(),
            SearchPlan::default(),
            "OR 不能只搜一个分支，否则漏命中"
        );
    }
    #[test]
    fn empty_page_advances_and_later_hits_do_not_repeat() {
        let q = query(vec![Condition::Size {
            min: Some(500),
            max: None,
            negate: false,
        }]);
        let c = Cursor::start(1, &q, None).expect("有效查询");
        let mut page = PageScan::new(&q, c, 2);
        for id in (10..13).rev() {
            let mut h = hit();
            h.message = id;
            assert!(page.push(h).expect("前进"), "空命中页仍须扫描");
        }
        let first = page.finish(false).expect("编码游标");
        assert!(first.hits.is_empty(), "没有文件不应误命中大小条件");
        assert!(first.cursor.is_some(), "空页不意味着对话开头");
        assert_eq!(first.scanned, 3, "扫描数不是命中数");
        let c = Cursor::start(1, &q, first.cursor.as_deref()).expect("续页");
        let mut second = PageScan::new(&q, c.clone(), 2);
        let mut h = hit();
        h.message = 9;
        h.file_size = Some(500);
        assert!(
            second.push(h.clone()).expect("新消息"),
            "未满命中上限应继续"
        );
        h.message = 8;
        assert!(
            !second.push(h).expect("第二命中"),
            "达到命中 limit 应停止而不是丢弃缓冲后面的消息"
        );
        let out = second.finish(false).expect("第二页");
        assert_eq!(out.scanned, 5, "续页扫描数须累加");
        assert_eq!(
            out.hits.iter().map(|h| h.message).collect::<Vec<_>>(),
            vec![9, 8],
            "新旧顺序不能颠倒"
        );
        let mut bad = PageScan::new(&q, c, 2);
        let mut h = hit();
        h.message = 10;
        assert!(bad.push(h).is_err(), "重复 offset 不能无限翻页");
    }
    #[test]
    fn leaf_negation_and_and_group_follow_boolean_semantics() {
        let empty = hit();
        for c in [
            Condition::Ext {
                exts: vec![String::from("mkv")],
                negate: true,
            },
            Condition::Size {
                min: Some(1),
                max: None,
                negate: true,
            },
        ] {
            assert!(
                query(vec![c]).matches(&empty),
                "无文件对取反 ext/size 都应命中"
            );
        }
        let q = query(vec![Condition::Group {
            r#match: Match::All,
            negate: true,
            conditions: vec![
                Condition::Text {
                    text: String::from("report"),
                    negate: false,
                },
                Condition::Media {
                    types: vec![MediaKind::Video],
                    negate: false,
                },
            ],
        }]);
        assert!(
            q.matches(&empty),
            "不全满足不是都不满足：只有文字满足也须命中"
        );
        let mut video = empty;
        video.media = Some(MediaKind::Video);
        video.has_link = true;
        assert!(!q.matches(&video), "两条都满足的组取反必须排除");
        assert!(
            query(vec![Condition::Media {
                types: vec![MediaKind::Link],
                negate: false
            }])
            .matches(&video),
            "视频同时含 URL 也须满足 link"
        );
    }
    #[test]
    fn cursor_bound_to_query_and_chat_and_all_except() {
        let q = query(vec![Condition::Text {
            text: String::from("report"),
            negate: false,
        }]);
        let mut c = Cursor::start(1, &q, None).expect("有效查询");
        c.before = 7;
        c.scanned = 300;
        let raw = c.encode().expect("编码");
        assert_eq!(
            Cursor::start(1, &q, Some(&raw)).expect("原上下文").scanned,
            300,
            "续页必须累计扫描进度"
        );
        assert!(
            Cursor::start(2, &q, Some(&raw)).is_err(),
            "跨对话游标必须拒绝"
        );
        let other = query(vec![Condition::Text {
            text: String::from("other"),
            negate: false,
        }]);
        assert!(
            Cursor::start(1, &other, Some(&raw)).is_err(),
            "换查询不得复用旧游标"
        );
        let s = Selection::All {
            except: vec![String::from("tg:1:7")],
        };
        assert!(!s.includes("tg:1:7"), "全选例外不能执行");
        assert!(s.includes("tg:1:8"), "未加载的匹配项仍在全选范围");
        assert!(s.validate(2).is_err(), "不得选择另一个对话的 id");
    }
}
