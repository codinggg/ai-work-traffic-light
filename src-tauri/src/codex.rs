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

const RECENT_ROLLOUT_SECS: u64 = 600;
const INITIAL_HEAD_BYTES: u64 = 64 * 1024;
const INITIAL_SCAN_BYTES: u64 = 1024 * 1024;

/// 监视器：记录每个 rollout 文件已读到的字节偏移，每轮只处理新增行。
pub struct CodexWatcher {
    sessions_dir: Option<PathBuf>,
    offsets: HashMap<PathBuf, u64>,
    /// 每个 rollout 文件已知的工作目录(cwd)：从 session_meta/turn_context 行抓到后持久记住，
    /// 供后续(常落在别的 poll chunk 里的)task 事件设项目名(cwd 末段)用。
    cwds: HashMap<PathBuf, String>,
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
            cwds: HashMap::new(),
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
                return self.catch_up_recent_file(path, &meta, len, store);
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

        self.apply_lines(path, &buf[..consume], store)
    }

    fn catch_up_recent_file(
        &mut self,
        path: &Path,
        meta: &std::fs::Metadata,
        len: u64,
        store: &mut Store,
    ) -> bool {
        let recent = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|elapsed| elapsed.as_secs() <= RECENT_ROLLOUT_SECS)
            .unwrap_or(false);

        if !recent || len == 0 {
            self.offsets.insert(path.to_path_buf(), len);
            return false;
        }

        // 首次见到该文件：扫描最近内容，只取【最后一个 task 事件】和 cwd —— 不逐个回放历史，
        // 否则会把早已完成的旧会话当成"刚结束"显示红灯（启动就闪红）。
        let start = len.saturating_sub(INITIAL_SCAN_BYTES);
        let mut last: Option<&'static str> = None;
        let mut cwd: Option<String> = None;
        if start > 0 {
            let (l, c) = self.scan_state(path, 0, len.min(INITIAL_HEAD_BYTES), false);
            last = l.or(last);
            cwd = c.or(cwd);
        }
        let (l, c) = self.scan_state(path, start, len, start > 0);
        last = l.or(last);
        cwd = c.or(cwd);

        self.offsets.insert(path.to_path_buf(), len);
        if let Some(c) = cwd {
            self.cwds.insert(path.to_path_buf(), c);
        }

        // 仅当 Codex 当前【正在干活】(最后一个事件是 task_started)才在启动时显示绿灯；
        // 历史上已完成(task_complete)的旧会话不在启动时亮红。
        if last == Some("task_started") {
            let session = session_key(path);
            let cwd = self.cwds.get(path).map(|s| s.as_str());
            store.apply("PreToolUse", &session, cwd);
            store.touch(&session);
            return true;
        }
        false
    }

    /// 扫描文件 [start,len) 区间，返回该段里【最后一个 task 事件】与【最后一个 cwd】(不写状态机)。
    /// 用于启动 catch_up：判断 Codex 当前到底在不在干活，而不回放历史完成事件。
    fn scan_state(
        &self,
        path: &Path,
        start: u64,
        len: u64,
        drop_first_partial_line: bool,
    ) -> (Option<&'static str>, Option<String>) {
        use std::io::{Read, Seek, SeekFrom};
        let mut last: Option<&'static str> = None;
        let mut cwd: Option<String> = None;
        let Ok(mut f) = std::fs::File::open(path) else {
            return (last, cwd);
        };
        if f.seek(SeekFrom::Start(start)).is_err() {
            return (last, cwd);
        }
        let mut bytes = Vec::new();
        if f.take(len - start).read_to_end(&mut bytes).is_err() {
            return (last, cwd);
        }
        let buf = String::from_utf8_lossy(&bytes);
        let consume = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if consume == 0 {
            return (last, cwd);
        }
        let mut lines = &buf[..consume];
        if drop_first_partial_line {
            let Some(nl) = lines.find('\n') else {
                return (last, cwd);
            };
            lines = &lines[nl + 1..];
        }
        for line in lines.lines() {
            if let Some(c) = extract_cwd(line) {
                cwd = Some(c);
            }
            if let Some(e) = codex_event(line) {
                last = Some(e);
            }
        }
        (last, cwd)
    }

    /// 处理一段(新增的)rollout 文本：抓 cwd 持久记住，识别 task_started/complete 喂状态机。
    /// cwd 跨 poll 持久化(self.cwds)——一个 turn 里 cwd 行(turn_context)和 task 行常落在不同的
    /// poll chunk，必须记住才能给 task 事件带上正确项目名(cwd 末段)。
    fn apply_lines(&mut self, path: &Path, lines: &str, store: &mut Store) -> bool {
        let session = session_key(path);
        let mut changed = false;
        for line in lines.lines() {
            if let Some(c) = extract_cwd(line) {
                self.cwds.insert(path.to_path_buf(), c);
            }
            match codex_event(line) {
                Some("task_started") => {
                    let cwd = self.cwds.get(path).map(|s| s.as_str());
                    eprintln!("[traffic-light][codex] task_started {session} cwd={cwd:?} -> working(绿)");
                    store.apply("PreToolUse", &session, cwd); // -> working(绿)，带项目名
                    changed = true;
                }
                Some("task_complete") => {
                    let cwd = self.cwds.get(path).map(|s| s.as_str());
                    eprintln!("[traffic-light][codex] task_complete {session} cwd={cwd:?} -> blocked(红)");
                    store.apply("Stop", &session, cwd); // -> 一轮结束(红灯)，带项目名
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

/// 从一行 rollout JSON 里取 cwd(工作目录)：session_meta / turn_context 行里有它。
/// 用它给 Codex 会话设项目名(cwd 末段)，前台标题匹配才认得出对应窗口。
/// 廉价子串预筛后再解析；递归找第一个 "cwd" 字符串字段(兼容嵌套在 payload 里)。
fn extract_cwd(line: &str) -> Option<String> {
    if !line.contains("\"cwd\"") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    find_cwd(&value)
}

fn find_cwd(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get("cwd") {
                if !s.is_empty() {
                    return Some(s.clone());
                }
            }
            map.values().find_map(find_cwd)
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(find_cwd),
        _ => None,
    }
}

/// 一行 rollout JSON 是否是 task_started / task_complete 事件。
/// 廉价子串预筛，避免每行都做完整 JSON 解析(日志行可能很大)。
fn codex_event(line: &str) -> Option<&'static str> {
    let complete = line.contains("\"task_complete\"");
    let started = line.contains("\"task_started\"");
    if !complete && !started {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
        return None;
    }
    match value
        .get("payload")
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str())
    {
        Some("task_complete") => Some("task_complete"),
        Some("task_started") => Some("task_started"),
        _ => None,
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
    use super::*;
    use crate::state::Store;

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
        assert_eq!(
            codex_event(
                r#"{"timestamp":"..", "type": "event_msg", "payload": {"type": "task_started"}}"#
            ),
            Some("task_started")
        );
        assert_eq!(
            codex_event(
                r#"{"type":"event_msg","payload":{"type":"agent_message","message":"task_complete"}}"#
            ),
            None
        );
    }

    #[test]
    fn codex_cwd_sets_project_label() {
        // Codex 的 cwd 在 session_meta/turn_context 行里 -> 应取末段作项目名，红灯才带得上、
        // 前台标题匹配才认得出对应窗口。
        let mut w = CodexWatcher {
            sessions_dir: None,
            offsets: HashMap::new(),
            cwds: HashMap::new(),
        };
        let path = Path::new("rollout-abc.jsonl");
        let mut store = Store::default();
        let lines = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"D:\\Users\\me\\Documents\\Playground"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
            "\n",
        );
        w.apply_lines(path, lines, &mut store);
        let agg = store.aggregate();
        assert_eq!(agg.status, "blocked"); // task_complete -> 红灯
        assert_eq!(agg.session_label, "Playground"); // 项目名取自 cwd 末段
        assert_eq!(agg.source, "codex");
    }

    #[test]
    fn codex_cwd_persists_across_polls() {
        // 一个 turn 里 cwd 行和 task 行常落在不同 poll chunk：cwd 必须跨调用持久化。
        let mut w = CodexWatcher {
            sessions_dir: None,
            offsets: HashMap::new(),
            cwds: HashMap::new(),
        };
        let path = Path::new("rollout-xyz.jsonl");
        let mut store = Store::default();
        // 第一次 poll：只有 turn_context(带 cwd)，没有 task 事件。
        w.apply_lines(
            path,
            r#"{"type":"turn_context","payload":{"cwd":"/home/u/MyProj"}}"#,
            &mut store,
        );
        // 第二次 poll：task_complete，cwd 行已不在本 chunk -> 应从持久化的 cwds 里取到 MyProj。
        w.apply_lines(
            path,
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
            &mut store,
        );
        let agg = store.aggregate();
        assert_eq!(agg.status, "blocked");
        assert_eq!(agg.session_label, "MyProj");
    }

    #[test]
    fn first_seen_recent_file_catches_up_current_state() {
        use std::io::Write;

        let path = std::env::temp_dir().join(format!(
            "aiwtl_codex_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n"
            ),
        )
        .unwrap();

        let mut watcher = CodexWatcher {
            sessions_dir: None,
            offsets: HashMap::new(),
            cwds: HashMap::new(),
        };
        let mut store = Store::default();

        assert!(watcher.tail_file(&path, &mut store));
        assert_eq!(store.aggregate().status, "working");

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            f,
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\"}}}}"
        )
        .unwrap();

        assert!(watcher.tail_file(&path, &mut store));
        assert_eq!(store.aggregate().status, "blocked"); // task_complete=一轮结束 -> 红灯

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn first_seen_completed_file_does_not_alert() {
        // 启动时遇到一个已完成(最后是 task_complete)的旧 rollout：不应回放历史亮红灯。
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "aiwtl_codex_done_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\"}}}}").unwrap();
        writeln!(f, "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\"}}}}").unwrap();
        drop(f);

        let mut watcher = CodexWatcher {
            sessions_dir: None,
            offsets: HashMap::new(),
            cwds: HashMap::new(),
        };
        let mut store = Store::default();
        // 首次见到(catch_up)：最后是 task_complete -> 不显示 -> 状态 none，不亮红。
        assert!(!watcher.tail_file(&path, &mut store));
        assert_eq!(store.aggregate().status, "none");

        let _ = std::fs::remove_file(&path);
    }
}
