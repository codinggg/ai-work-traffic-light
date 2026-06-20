// Codex 接入：监视 ~/.codex/sessions 下的 rollout 会话日志(JSONL)来感知工作状态。
//
// 为什么不用 Codex 的 `notify`：那是单值配置，常被 Codex 自己的 computer-use 插件占用，
// 抢过来会破坏它、且 Codex 可能改回去。监视日志则零配置、不冲突，CLI 和 VSCode 都会写。
//
// 事件(取自每行的 event_msg.payload.type)：
//   task_started  -> working(绿，正在干活)
//   task_complete -> idle(黄，一轮跑完，该你了)
// 日志里没有可靠的"等待批准"标记，所以 Codex 只有绿/黄，没有红灯。
// Codex 没有"会话结束"信号；靠 Store::expire 让停掉的会话过一会自动消隐。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::state::Store;

/// 监视器：记录每个 rollout 文件已读到的字节偏移，每轮只处理新增行。
pub struct CodexWatcher {
    sessions_dir: Option<PathBuf>,
    offsets: HashMap<PathBuf, u64>,
}

impl Default for CodexWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexWatcher {
    pub fn new() -> Self {
        let dir = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|h| PathBuf::from(h).join(".codex").join("sessions"));
        Self {
            sessions_dir: dir,
            offsets: HashMap::new(),
        }
    }

    /// 轮询活跃 rollout 文件的新增行，把 Codex 状态喂进状态机。返回是否有状态更新。
    pub fn poll(&mut self, store: &mut Store) -> bool {
        let Some(dir) = self.sessions_dir.clone() else {
            return false;
        };
        if !dir.is_dir() {
            return false;
        }
        let mut changed = false;
        for file in self.active_files(&dir) {
            changed |= self.tail_file(&file, store);
        }
        changed
    }

    /// 候选文件 = 最新一天目录下的 *.jsonl ∪ 已在跟踪(且仍存在)的文件。
    /// 只下钻到最新的 年/月/日 目录，避免每轮遍历历史全树。
    fn active_files(&self, dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self.offsets.keys().filter(|p| p.exists()).cloned().collect();
        if let Some(day) = latest_day_dir(dir) {
            if let Ok(rd) = std::fs::read_dir(&day) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("jsonl") && !files.contains(&p)
                    {
                        files.push(p);
                    }
                }
            }
        }
        files
    }

    /// 读取单个文件自上次偏移后的新增完整行，识别 task_started/complete 喂状态机。
    fn tail_file(&mut self, path: &Path, store: &mut Store) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let len = meta.len();
        let start = match self.offsets.get(path).copied() {
            Some(p) => p,
            None => {
                // 首次见到：定位到末尾，只看之后的新增(不回放历史)。
                self.offsets.insert(path.to_path_buf(), len);
                return false;
            }
        };
        if len <= start {
            if len < start {
                self.offsets.insert(path.to_path_buf(), len); // 被截断/轮转，重置
            }
            return false;
        }
        let Ok(mut f) = std::fs::File::open(path) else {
            return false;
        };
        if f.seek(SeekFrom::Start(start)).is_err() {
            return false;
        }
        let mut buf = String::new();
        if f.take(len - start).read_to_string(&mut buf).is_err() {
            return false; // 末尾可能切到半个 UTF-8 字符，下轮再读
        }
        // 只消费到最后一个换行，半行留到下轮补齐。
        let consume = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if consume == 0 {
            return false;
        }
        self.offsets.insert(path.to_path_buf(), start + consume as u64);

        let session = session_key(path);
        let mut changed = false;
        for line in buf[..consume].lines() {
            match codex_event(line) {
                Some("task_started") => {
                    store.apply("PreToolUse", &session, None); // -> working(绿)
                    changed = true;
                }
                Some("task_complete") => {
                    store.apply("Stop", &session, None); // -> idle(黄)
                    changed = true;
                }
                _ => {}
            }
        }
        // 有任何新增行就刷新该会话活跃时间，避免长任务(久未 complete)被 expire 误清。
        store.touch(&session);
        changed
    }
}

/// 该 rollout 文件对应的会话 key（用文件名里的 session uuid，稳定且各会话互不相同）。
fn session_key(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("codex");
    format!("codex:{stem}")
}

/// 一行 rollout JSON 是否是 task_started / task_complete 事件。
/// 廉价子串预筛，避免每行都做完整 JSON 解析(日志行可能很大)。
fn codex_event(line: &str) -> Option<&'static str> {
    let complete = line.contains("\"type\":\"task_complete\"");
    let started = line.contains("\"type\":\"task_started\"");
    if !complete && !started {
        return None;
    }
    // 确认是 event_msg 的 payload（避免误命中别处文本里的同名字样）。
    if !line.contains("\"type\":\"event_msg\"") {
        return None;
    }
    if complete {
        Some("task_complete")
    } else {
        Some("task_started")
    }
}

/// 从 sessions/ 下钻到最新的 年/月/日 目录（目录名按字典序即时间序，取最大）。
fn latest_day_dir(sessions: &Path) -> Option<PathBuf> {
    let mut cur = sessions.to_path_buf();
    for _ in 0..3 {
        cur = max_subdir(&cur)?;
    }
    Some(cur)
}

/// 取 dir 下名字最大的子目录。
fn max_subdir(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

#[cfg(test)]
mod tests {
    use super::codex_event;

    #[test]
    fn recognizes_task_events_only_in_event_msg() {
        let started = r#"{"timestamp":"..","type":"event_msg","payload":{"type":"task_started"}}"#;
        let complete =
            r#"{"timestamp":"..","type":"event_msg","payload":{"type":"task_complete","turn_id":"x"}}"#;
        assert_eq!(codex_event(started), Some("task_started"));
        assert_eq!(codex_event(complete), Some("task_complete"));
        // 普通消息行不应命中。
        assert_eq!(
            codex_event(r#"{"type":"response_item","payload":{"type":"message"}}"#),
            None
        );
        // 即便文本里出现 task_complete 字样，但不是 event_msg，也不命中。
        assert_eq!(
            codex_event(r#"{"type":"response_item","payload":{"type":"task_complete"}}"#),
            None
        );
    }
}
