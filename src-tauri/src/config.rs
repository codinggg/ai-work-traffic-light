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
    pub vertical_layout: bool,
    /// 窗口位置(物理像素) (x, y)；None 表示还没存过 -> 首次显示时自动贴任务栏。
    pub pos: Option<(i32, i32)>,
    /// 横向红绿灯窗口大小(逻辑像素)。None 表示使用程序内置默认大小。
    pub horizontal_size: Option<(f64, f64)>,
    /// 竖向红绿灯窗口大小(逻辑像素)。None 表示使用程序内置默认大小。
    pub vertical_size: Option<(f64, f64)>,
    /// 自定义提示音：普通状态切换(绿/黄)用。存 audio/ 下的文件名(如 "ding.wav")，
    /// 也兼容写绝对路径。None/解析不到文件则用系统提示音。
    pub sound_file: Option<String>,
    /// 自定义提示音：红灯(等你确认)用，可设更显眼的。规则同 sound_file。None 用系统警告音。
    pub sound_urgent_file: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            locked: false,
            vertical_layout: false,
            pos: None,
            horizontal_size: None,
            vertical_size: None,
            sound_file: None,
            sound_urgent_file: None,
        }
    }
}

/// exe 所在目录。
fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// exe 同目录下的 config.json 路径。
fn config_path() -> Option<PathBuf> {
    Some(exe_dir()?.join("config.json"))
}

/// exe 同目录下存放自定义提示音的 audio/ 文件夹。
pub fn audio_dir() -> Option<PathBuf> {
    Some(exe_dir()?.join("audio"))
}

/// 诊断用：exe 同目录的 events.log。记录 app 收到的每个 Claude hook 事件，
/// 用来排查"某操作灯色不对"——看清到底触发了哪些事件、顺序与间隔。
/// 这是临时诊断功能，问题定位后可移除。
pub fn debug_log_path() -> Option<PathBuf> {
    Some(exe_dir()?.join("events.log"))
}

/// 启动时确保 audio/ 文件夹存在（不存在就建）。
pub fn ensure_audio_dir() {
    if let Some(d) = audio_dir() {
        let _ = std::fs::create_dir_all(d);
    }
}

/// 把 config 里的提示音取值解析成真实存在的文件路径：
/// - 空 -> None；
/// - 含路径分隔符/绝对路径：原样用(存在才返回)，兼容外部绝对路径；
/// - 否则当作 audio/ 下的文件名。
pub fn resolve_sound(value: &str) -> Option<PathBuf> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if std::path::Path::new(v).is_absolute() || v.contains('/') || v.contains('\\') {
        let p = PathBuf::from(v);
        return p.exists().then_some(p);
    }
    let f = audio_dir()?.join(v);
    f.exists().then_some(f)
}

/// 把用户选中的音频复制进 audio/，返回文件名(config 里存这个名字)。
/// 若来源已经就在 audio/ 里，则只返回文件名、不重复拷贝。
pub fn import_sound(src: &std::path::Path) -> Option<String> {
    let name = src.file_name()?.to_string_lossy().to_string();
    let dir = audio_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let dest = dir.join(&name);
    let same = src
        .canonicalize()
        .ok()
        .zip(dest.canonicalize().ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if !same && std::fs::copy(src, &dest).is_err() {
        return None;
    }
    Some(name)
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

/// 启动时确保配置文件存在：exe 同目录没有 config.json 就用当前值(首次即默认值)新建一个。
pub fn ensure(cfg: &Config) {
    if let Some(path) = config_path() {
        if !path.exists() {
            save(cfg);
        }
    }
}
