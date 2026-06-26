// Antigravity 接入：监视 ~/.gemini/antigravity-ide/brain/<uuid>/.system_generated/logs/transcript.jsonl 
// 来感知工作状态，因为 Antigravity 没有完善的 hooks。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::state::Store;

const IDLE_THRESHOLD_SECS: u64 = 15;

/// 监视器：记录每个 transcript 的字节偏移和最后一次写入时间。
pub struct AntigravityWatcher {
    brain_dir: Option<PathBuf>,
    offsets: HashMap<PathBuf, u64>,
    last_update: HashMap<String, Instant>,
    active_sessions: std::collections::HashSet<String>,
}

impl Default for AntigravityWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityWatcher {
    pub fn new() -> Self {
        let dir = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|h| PathBuf::from(h).join(".gemini").join("antigravity-ide").join("brain"));
        Self {
            brain_dir: dir,
            offsets: HashMap::new(),
            last_update: HashMap::new(),
            active_sessions: std::collections::HashSet::new(),
        }
    }

    pub fn poll(&mut self, store: &mut Store) -> bool {
        let Some(dir) = self.brain_dir.clone() else {
            return false;
        };
        if !dir.is_dir() {
            return false;
        }

        let mut changed = false;
        
        // 查找文件
        for file in self.active_files(&dir) {
            changed |= self.tail_file(&file, store);
        }

        // 检查超时转黄灯
        let now = Instant::now();
        let mut to_idle = Vec::new();
        for session_id in &self.active_sessions {
            if let Some(t) = self.last_update.get(session_id) {
                if now.duration_since(*t).as_secs() >= IDLE_THRESHOLD_SECS {
                    to_idle.push(session_id.clone());
                }
            }
        }

        for session_id in to_idle {
            self.active_sessions.remove(&session_id);
            store.apply("Stop", &session_id, None); // -> idle (黄灯)
            changed = true;
        }

        changed
    }

    fn active_files(&self, dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self.offsets.keys().filter(|p| p.exists()).cloned().collect();
        // 扫描所有 brain 目录下的直接子文件夹
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let log_file = p.join(".system_generated").join("logs").join("transcript.jsonl");
                    if log_file.exists() && !files.contains(&log_file) {
                        files.push(log_file);
                    }
                }
            }
        }
        files
    }

    fn tail_file(&mut self, path: &Path, store: &mut Store) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let len = meta.len();
        let start = match self.offsets.get(path).copied() {
            Some(p) => p,
            None => {
                // 首次见到：记录偏移。
                self.offsets.insert(path.to_path_buf(), len);
                
                // 如果这是刚刚被修改的活跃文件（比如新建的对话），立刻触发绿灯，避免延迟。
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let is_recent = modified.elapsed().unwrap_or_default().as_secs() < 30;
                
                if is_recent {
                    let session = session_key(path);
                    if !self.active_sessions.contains(&session) {
                        store.apply("PreToolUse", &session, None);
                        self.active_sessions.insert(session.clone());
                        self.last_update.insert(session, Instant::now());
                        return true;
                    }
                }
                return false;
            }
        };

        if len <= start {
            if len < start {
                self.offsets.insert(path.to_path_buf(), len); // 截断或重建
            }
            return false;
        }

        // 有新增内容
        self.offsets.insert(path.to_path_buf(), len);

        let session = session_key(path);
        let mut changed = false;

        // 因为任何新增写入都意味着它正在活动（无论是模型输出还是工具执行）
        // 只要它写入了，我们就将其标记为 Working。
        if !self.active_sessions.contains(&session) {
            store.apply("PreToolUse", &session, None); // -> working (绿灯)
            self.active_sessions.insert(session.clone());
            changed = true;
        } else {
            // 如果已经在 active 状态里，我们也需要 store.touch 来刷新它的过期时间
            store.touch(&session);
        }

        self.last_update.insert(session, Instant::now());
        
        changed
    }
}

/// 从路径中提取 uuid，例如 .../brain/<uuid>/.system_generated/...
fn session_key(path: &Path) -> String {
    // path 类似 ~/.gemini/antigravity-ide/brain/<uuid>/.system_generated/logs/transcript.jsonl
    // 我们可以回退 3 级目录来拿到 uuid。
    let uuid = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    format!("antigravity:{uuid}")
}
