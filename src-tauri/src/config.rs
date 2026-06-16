// 设置持久化：把用户可改的设置存到 **exe 同目录** 的 config.json。
//
// 存什么：提示音开关(sound_enabled)、位置锁定(locked)、窗口位置(pos)。
//   - 开机自启由 autostart 插件自己写进系统(注册表)，不在这里存。
//   - 位置一起存：否则"锁定位置"重启后又被拽回任务栏默认处，前后矛盾。
// 存哪里：current_exe() 的所在目录。开发时是 target/debug/config.json；
//   便携式分发时就在 exe 旁边。目录不可写(如装在 Program Files)时静默跳过，
//   不致命——只是设置不持久。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub sound_enabled: bool,
    pub locked: bool,
    /// 窗口位置(物理像素) (x, y)；None 表示还没存过 -> 首次显示时自动贴任务栏。
    pub pos: Option<(i32, i32)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            locked: false,
            pos: None,
        }
    }
}

/// exe 同目录下的 config.json 路径。
fn config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("config.json"))
}

/// 读配置；文件不存在/解析失败都回落到默认值。
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// 写配置；失败(目录只读等)静默忽略。
pub fn save(cfg: &Config) {
    let Some(path) = config_path() else {
        return;
    };
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(&path, s);
    }
}
