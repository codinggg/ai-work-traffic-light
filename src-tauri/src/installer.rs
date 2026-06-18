// U5: hook 安装器。
//
// 把"状态上报"hook 合并写入用户全局 ~/.claude/settings.json，让所有项目/会话
// 都覆盖、无需逐仓库配置。每个事件注册一条命令：用 curl 把 hook 的 stdin
// 负载 POST 到本地端点 /event/<EventName>。
//
// 设计要点：
//   - 幂等：已存在我们的命令则不重复添加。
//   - 安全：写入前备份为 settings.json.bak；保留用户already有的其它 hooks/设置。
//   - 可卸载：按本地端点 URL 特征移除我们加的条目。

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::STATE_PORT;

/// (事件名, 是否需要 matcher)。PreToolUse/PostToolUse 需要 matcher "" 匹配全部工具。
const EVENTS: &[(&str, bool)] = &[
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Notification", false),
    ("Stop", false),
    ("SessionEnd", false),
];

fn settings_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("找不到用户主目录 (USERPROFILE/HOME)")?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}

fn our_command(event: &str) -> String {
    format!(
        "curl -s -X POST --data-binary @- http://127.0.0.1:{STATE_PORT}/event/{event}"
    )
}

fn url_marker() -> String {
    format!("127.0.0.1:{STATE_PORT}/event/")
}

/// 把我们的 hooks 合并进 settings.json。返回给用户看的结果说明。
pub fn install() -> Result<String, String> {
    let path = settings_path()?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut root: Value = if path.exists() {
        let txt = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("json.bak"), &txt).ok(); // 备份
        serde_json::from_str(&txt).map_err(|e| format!("settings.json 解析失败: {e}"))?
    } else {
        json!({})
    };

    let obj = root.as_object_mut().ok_or("settings.json 顶层不是对象")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks 字段不是对象")?;

    let mut added = 0;
    for (event, needs_matcher) in EVENTS {
        let cmd = our_command(event);
        let arr = hooks
            .entry((*event).to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{event} 不是数组"))?;

        let exists = arr.iter().any(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hs| {
                    hs.iter()
                        .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(cmd.as_str()))
                })
        });
        if exists {
            continue;
        }

        let mut group = json!({ "hooks": [ { "type": "command", "command": cmd } ] });
        if *needs_matcher {
            group
                .as_object_mut()
                .unwrap()
                .insert("matcher".into(), json!(""));
        }
        arr.push(group);
        added += 1;
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(format!(
        "已写入 {}\n新增 {} 个事件 hook。请重启 Claude Code 生效。",
        path.display(),
        added
    ))
}

/// 移除我们加的 hook 条目（按本地端点 URL 特征匹配）。
pub fn uninstall() -> Result<String, String> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok("settings.json 不存在，无需卸载。".into());
    }
    let txt = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut root: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    let marker = url_marker();
    let mut removed = 0;

    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for val in hooks.values_mut() {
            if let Some(arr) = val.as_array_mut() {
                let before = arr.len();
                arr.retain(|group| {
                    let ours = group
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hs| {
                            hs.iter().any(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains(&marker))
                            })
                        });
                    !ours
                });
                removed += before - arr.len();
            }
        }
    }

    let out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(format!(
        "已移除 {removed} 个 hook 条目。请重启 Claude Code 生效。"
    ))
}

// ===== Codex 接入 =====
//
// Codex CLI 的外部钩子是 ~/.codex/config.toml 里的顶层 `notify`：它会用
// `notify` 指定的程序 + 把事件 JSON 作为最后一个参数来调用。我们让它直接调本程序自身：
//   notify = ["<本程序绝对路径>", "--codex-notify"]
// 程序检测到该 flag 就把 JSON 转发到本地端点(见 main.rs codex_notify_relay)，不启动 GUI。
// 数组形式由 Codex 直接 exec（不过 shell），路径含空格也安全。
// 目前 Codex 只发 agent-turn-complete 事件，对应黄灯「该你了」。

/// 识别"这条 notify 是我们装的"的标记参数。
const CODEX_FLAG: &str = "--codex-notify";

fn codex_config_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("找不到用户主目录 (USERPROFILE/HOME)")?;
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

/// notify 数组是否已是我们装的（含 CODEX_FLAG）。
fn codex_notify_is_ours(doc: &toml_edit::DocumentMut) -> bool {
    doc.get("notify")
        .and_then(|i| i.as_array())
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(CODEX_FLAG)))
}

/// 把 Codex 的 notify 指向本程序。保留 config.toml 里用户原有的其它配置/注释。
pub fn install_codex() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("取程序路径失败: {e}"))?
        .to_string_lossy()
        .to_string();

    let path = codex_config_path()?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut doc: toml_edit::DocumentMut = if path.exists() {
        let txt = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("toml.bak"), &txt).ok(); // 备份
        txt.parse()
            .map_err(|e| format!("config.toml 解析失败: {e}"))?
    } else {
        toml_edit::DocumentMut::new()
    };

    if codex_notify_is_ours(&doc) {
        return Ok(format!("{} 已配置过，无需重复。", path.display()));
    }

    let mut arr = toml_edit::Array::new();
    arr.push(exe.as_str());
    arr.push(CODEX_FLAG);
    doc["notify"] = toml_edit::value(arr);

    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    Ok(format!(
        "已写入 {}\nnotify 已指向本程序。请重启 Codex 生效。",
        path.display()
    ))
}

/// 移除我们装的 Codex notify（仅当它是我们的；用户自己的 notify 不动）。
pub fn uninstall_codex() -> Result<String, String> {
    let path = codex_config_path()?;
    if !path.exists() {
        return Ok("config.toml 不存在，无需卸载。".into());
    }
    let txt = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = txt
        .parse()
        .map_err(|e| format!("config.toml 解析失败: {e}"))?;

    if !codex_notify_is_ours(&doc) {
        return Ok("config.toml 里的 notify 不是本程序装的，未改动。".into());
    }
    std::fs::write(path.with_extension("toml.bak"), &txt).ok(); // 备份
    doc.remove("notify");
    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    Ok("已移除 Codex notify。请重启 Codex 生效。".into())
}
