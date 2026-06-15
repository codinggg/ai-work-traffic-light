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
}

impl Status {
    fn urgency(self) -> u8 {
        match self {
            Status::Blocked => 3,
            Status::Idle => 2,
            Status::Working => 1,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Idle => "idle",
            Status::Blocked => "blocked",
        }
    }
}

struct Session {
    status: Status,
    label: String,
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
        });
        entry.status = status;
        if let Some(l) = label {
            entry.label = l;
        }
    }

    /// 计算当前应显示的聚合状态。
    pub fn aggregate(&self) -> Aggregate {
        let top = self.sessions.values().map(|s| s.status).max_by_key(|s| s.urgency());
        match top {
            None => Aggregate {
                status: "none".into(),
                session_label: String::new(),
            },
            Some(st) => {
                // 仅红灯附带"是哪个会话"(挑一个 blocked 会话的标识)。
                let label = if st == Status::Blocked {
                    self.sessions
                        .values()
                        .find(|s| s.status == Status::Blocked)
                        .map(|s| s.label.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                Aggregate {
                    status: st.as_str().into(),
                    session_label: label,
                }
            }
        }
    }
}

/// 从 cwd 取最后一段作为会话标识(项目目录名)，兼容 / 与 \。
fn label_from_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(|c| c == '/' || c == '\\');
    trimmed
        .rsplit(|c| c == '/' || c == '\\')
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
}
