// U4: 会话状态机 + 多会话聚合。
//
// 每个 Claude Code 会话(session_id)维护一个状态；悬浮灯显示所有会话里
// 最紧急的那个(blocked > idle > working)，无会话则隐藏。红灯时附带需要
// 处理的那个会话的标识(取自 cwd 的项目目录名)。
//
// 事件→状态映射(KTD1，待 U1 spike 实测校准)：
//   UserPromptSubmit / PreToolUse / PostToolUse -> working(绿)
//   Notification                                -> blocked(红)
//   Stop / SubagentStop                         -> idle(黄)
//   SessionEnd                                  -> 移除该会话

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Working,
    Idle,
    Blocked,
    /// API 报错(如 429 限流/服务不可用)。hooks 不报这个，靠扫 transcript 发现。
    ApiError,
}

impl Status {
    fn urgency(self) -> u8 {
        match self {
            Status::Blocked => 3,
            // API 错误与 idle 同档(都黄灯、都该你关注一下)，但都高于 working。
            Status::ApiError => 2,
            Status::Idle => 2,
            Status::Working => 1,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Idle => "idle",
            Status::Blocked => "blocked",
            Status::ApiError => "error",
        }
    }
}

struct Session {
    status: Status,
    label: String,
    /// transcript 里检测到 API 错误(如 429)。下一个正常 hook 事件会清掉它。
    api_error: bool,
    /// 该会话的 transcript 文件路径(来自 hook 负载的 transcript_path)。
    transcript: Option<String>,
    /// transcript 已读到的字节偏移；None = 还没初始化(首次定位到文件末尾，只看新增)。
    tpos: Option<u64>,
    /// 最近一次更新时间。用于 Codex 会话(无 SessionEnd)的自动过期。
    updated: std::time::Instant,
}

impl Session {
    /// 对外展示用的实际状态：有 API 错误时盖过事件状态(显示 error/黄灯)。
    fn effective(&self) -> Status {
        if self.api_error {
            Status::ApiError
        } else {
            self.status
        }
    }
}

#[derive(Default)]
pub struct Store {
    sessions: HashMap<String, Session>,
}

/// 推送给前端的聚合状态。字段名与前端 main.js 约定一致。
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Aggregate {
    pub status: String,
    #[serde(rename = "sessionLabel")]
    pub session_label: String,
    /// 当前在"催你"的那个会话来自哪个工具："claude" / "codex" / ""(无)。
    /// 用于"精确到窗口"的停闪判断：只有切到该来源对应的窗口才算已查看。
    pub source: String,
}

impl Store {
    /// 应用一个 hook 事件，更新对应会话的状态。
    pub fn apply(&mut self, event: &str, session_id: &str, cwd: Option<&str>) {
        match event {
            "Notification" => self.set(session_id, Status::Blocked, cwd),
            "Stop" | "SubagentStop" => self.set(session_id, Status::Idle, cwd),
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => {
                self.set(session_id, Status::Working, cwd)
            }
            "SessionEnd" => {
                self.sessions.remove(session_id);
            }
            _ => {} // SessionStart 等暂不建状态(等首个活动事件)
        }
    }

    fn set(&mut self, id: &str, status: Status, cwd: Option<&str>) {
        let label = cwd.map(label_from_cwd);
        let entry = self.sessions.entry(id.to_string()).or_insert_with(|| Session {
            status,
            label: label.clone().unwrap_or_else(|| short_id(id)),
            api_error: false,
            transcript: None,
            tpos: None,
            updated: std::time::Instant::now(),
        });
        entry.status = status;
        entry.updated = std::time::Instant::now();
        // 收到任意正常事件 = Claude 已越过之前的 API 错误，清掉错误标记。
        entry.api_error = false;
        if let Some(l) = label {
            entry.label = l;
        }
    }

    /// 移除 id 以 `prefix` 开头、且超过 `max_age` 未更新的会话。返回是否有移除。
    /// 用于 Codex：它没有 SessionEnd 事件，靠这个让黄灯过一会儿自动消隐，不永久卡住。
    pub fn expire(&mut self, prefix: &str, max_age: std::time::Duration) -> bool {
        let before = self.sessions.len();
        self.sessions
            .retain(|id, s| !(id.starts_with(prefix) && s.updated.elapsed() >= max_age));
        before != self.sessions.len()
    }

    /// 该会话存在则刷新其活跃时间。Codex 监视器在会话有任何新增日志行时调用，
    /// 避免一个长任务(久未 task_complete)被 expire 误清。
    pub fn touch(&mut self, id: &str) {
        if let Some(s) = self.sessions.get_mut(id) {
            s.updated = std::time::Instant::now();
        }
    }

    /// 记录会话的 transcript 路径(来自 hook 的 transcript_path)。首次设置时把读取
    /// 偏移定位到文件当前末尾，只盯之后的新增内容(避免把历史里的旧错误当成新错误)。
    pub fn set_transcript(&mut self, id: &str, path: &str) {
        if let Some(s) = self.sessions.get_mut(id) {
            if s.transcript.as_deref() != Some(path) {
                s.transcript = Some(path.to_string());
                s.tpos = None; // 下次 scan 时定位到末尾
            }
        }
    }

    /// 轮询各会话 transcript 的新增内容，发现 API 错误就标记该会话。
    /// 返回是否有会话状态发生变化(用于决定是否刷新灯)。错误标记不在这里清除——
    /// 由下一个正常 hook 事件(apply)清除。
    pub fn scan_api_errors(&mut self) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        let mut changed = false;
        for s in self.sessions.values_mut() {
            let Some(path) = s.transcript.clone() else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let len = meta.len();
            let start = match s.tpos {
                Some(p) => p,
                None => {
                    s.tpos = Some(len); // 首次：跳过历史，只看新增
                    continue;
                }
            };
            if len < start {
                s.tpos = Some(len); // 文件被截断/轮转，重置
                continue;
            }
            if len == start {
                continue;
            }
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            if f.seek(SeekFrom::Start(start)).is_err() {
                continue;
            }
            let mut buf = String::new();
            if f.take(len - start).read_to_string(&mut buf).is_err() {
                continue; // 末尾可能切到半个 UTF-8 字符，下轮再读
            }
            // 只消费到最后一个换行，避免处理写了一半的行(下轮补齐)。
            let consume = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
            if consume == 0 {
                continue;
            }
            s.tpos = Some(start + consume as u64);
            let found = buf[..consume].lines().any(line_is_api_error);
            if found && !s.api_error {
                s.api_error = true;
                changed = true;
            }
        }
        changed
    }

    /// 计算当前应显示的聚合状态。
    pub fn aggregate(&self) -> Aggregate {
        // 取紧急度最高的那个会话(连同 id，用于判定来源 claude/codex)。
        let top = self
            .sessions
            .iter()
            .max_by_key(|(_, s)| s.effective().urgency());
        match top {
            None => Aggregate {
                status: "none".into(),
                session_label: String::new(),
                source: String::new(),
            },
            Some((id, s)) => {
                let st = s.effective();
                let source = if id.starts_with("codex:") {
                    "codex"
                } else {
                    "claude"
                };
                // 仅红灯附带"是哪个会话"。
                let label = if st == Status::Blocked {
                    s.label.clone()
                } else {
                    String::new()
                };
                Aggregate {
                    status: st.as_str().into(),
                    session_label: label,
                    source: source.into(),
                }
            }
        }
    }
}

/// 一行 transcript JSON 是否表示一条 API 错误记录(如 429/服务不可用)。
/// Claude Code 把这类错误写成 `"isApiErrorMessage":true` 的 assistant 条目。
fn line_is_api_error(line: &str) -> bool {
    line.contains("\"isApiErrorMessage\":true")
}

/// 从 cwd 取最后一段作为会话标识(项目目录名)，兼容 / 与 \。
fn label_from_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_session_working() {
        let mut s = Store::default();
        s.apply("UserPromptSubmit", "a", Some("e:/proj/foo"));
        let agg = s.aggregate();
        assert_eq!(agg.status, "working");
        assert_eq!(agg.session_label, "");
    }

    #[test]
    fn blocked_wins_and_carries_label() {
        let mut s = Store::default();
        s.apply("UserPromptSubmit", "a", Some("e:/proj/foo"));
        s.apply("Notification", "b", Some("e:/work/bar"));
        let agg = s.aggregate();
        assert_eq!(agg.status, "blocked");
        assert_eq!(agg.session_label, "bar");
        assert_eq!(agg.source, "claude");
    }

    #[test]
    fn codex_session_reports_codex_source() {
        let mut s = Store::default();
        s.apply("Stop", "codex:rollout-abc", Some("e:/x/foo"));
        let agg = s.aggregate();
        assert_eq!(agg.status, "idle");
        assert_eq!(agg.source, "codex");
    }

    #[test]
    fn idle_beats_working_no_label() {
        let mut s = Store::default();
        s.apply("PreToolUse", "a", Some("/x/foo"));
        s.apply("Stop", "b", Some("/x/bar"));
        let agg = s.aggregate();
        assert_eq!(agg.status, "idle");
        assert_eq!(agg.session_label, "");
    }

    #[test]
    fn recovery_blocked_to_working() {
        let mut s = Store::default();
        s.apply("Notification", "a", Some("/x/foo"));
        assert_eq!(s.aggregate().status, "blocked");
        // 授权放行后工具继续 -> PostToolUse 让它回到 working
        s.apply("PostToolUse", "a", Some("/x/foo"));
        assert_eq!(s.aggregate().status, "working");
    }

    #[test]
    fn session_end_removes_then_hidden() {
        let mut s = Store::default();
        s.apply("UserPromptSubmit", "a", Some("/x/y"));
        s.apply("SessionEnd", "a", None);
        assert_eq!(s.aggregate().status, "none");
    }

    #[test]
    fn label_from_cwd_handles_both_separators() {
        assert_eq!(label_from_cwd("e:/mycode/work/foo"), "foo");
        assert_eq!(label_from_cwd("e:\\mycode\\bar\\"), "bar");
        assert_eq!(label_from_cwd("/single"), "single");
    }

    #[test]
    fn expire_removes_only_matching_prefix_after_age() {
        let mut s = Store::default();
        s.apply("Stop", "codex:e:/x/foo", Some("e:/x/foo")); // Codex 会话(黄)
        s.apply("UserPromptSubmit", "claude-a", Some("/x/bar")); // Claude 会话
        // max_age=0 -> 立即过期：只清 codex: 前缀的，Claude 的保留。
        assert!(s.expire("codex:", std::time::Duration::from_secs(0)));
        assert_eq!(s.aggregate().status, "working"); // Claude 会话还在
        // 再次过期无 codex 会话可清 -> 无变化。
        assert!(!s.expire("codex:", std::time::Duration::from_secs(0)));
    }

    #[test]
    fn detects_api_error_line() {
        assert!(line_is_api_error(
            r#"{"type":"assistant","isApiErrorMessage":true,"apiErrorStatus":429}"#
        ));
        assert!(!line_is_api_error(
            r#"{"type":"assistant","isApiErrorMessage":false}"#
        ));
        assert!(!line_is_api_error(r#"{"type":"user"}"#));
    }

    #[test]
    fn api_error_in_transcript_shows_error_then_clears() {
        use std::io::Write;
        let path = std::env::temp_dir().join("aiwtl_test_transcript.jsonl");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "{\"type\":\"user\"}\n").unwrap();
        let p = path.to_string_lossy().to_string();

        let mut s = Store::default();
        s.apply("UserPromptSubmit", "a", Some("/x/foo"));
        s.set_transcript("a", &p);
        // 首次 scan 把偏移定位到末尾(跳过历史)，状态仍是 working。
        assert!(!s.scan_api_errors());
        assert_eq!(s.aggregate().status, "working");

        // 追加一条 API 错误记录 -> scan 后变 error(黄灯)。
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"isApiErrorMessage\":true,\"apiErrorStatus\":429}}").unwrap();
        assert!(s.scan_api_errors());
        assert_eq!(s.aggregate().status, "error");

        // 下一个正常事件清掉错误标记 -> 回到 working。
        s.apply("PostToolUse", "a", Some("/x/foo"));
        assert_eq!(s.aggregate().status, "working");

        let _ = std::fs::remove_file(&path);
    }
}
