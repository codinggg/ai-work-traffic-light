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
    // PreCompact: /compact(手动或自动)压缩上下文时触发 -> 灯亮绿(工作中)。
    ("PreCompact", false),
    ("SessionEnd", false),
];

fn settings_paths() -> Result<Vec<PathBuf>, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("找不到用户主目录 (USERPROFILE/HOME)")?;
    let base = PathBuf::from(home);
    Ok(vec![
        base.join(".claude").join("settings.json"),
        base.join(".antigravity").join("settings.json"),
        base.join(".antigravity-ide").join("settings.json"),
        base.join(".codex").join("settings.json"),
    ])
}

fn our_command(event: &str) -> String {
    format!(
        "curl -s -X POST --data-binary @- http://127.0.0.1:{STATE_PORT}/event/{event}"
    )
}

fn url_marker() -> String {
    format!("127.0.0.1:{STATE_PORT}/event/")
}

fn process_install_for_path(path: &PathBuf) -> Result<usize, String> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut root: Value = if path.exists() {
        let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("json.bak"), &txt).ok(); // 备份
        serde_json::from_str(&txt).map_err(|e| format!("{} 解析失败: {}", path.display(), e))?
    } else {
        json!({})
    };

    let obj = if let Some(o) = root.as_object_mut() { o } else { return Err(format!("{} 顶层不是对象", path.display())) };
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{} 的 hooks 字段不是对象", path.display()))?;

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
    std::fs::write(path, out).map_err(|e| e.to_string())?;
    Ok(added)
}

/// 把我们的 hooks 合并进 settings.json。返回给用户看的结果说明。
pub fn install() -> Result<String, String> {
    let paths = settings_paths()?;
    let mut total_added = 0;
    let mut msgs = Vec::new();

    for path in paths {
        match process_install_for_path(&path) {
            Ok(added) => {
                total_added += added;
                msgs.push(format!("已写入 {}", path.display()));
            }
            Err(e) => {
                msgs.push(format!("失败 {}: {}", path.display(), e));
            }
        }
    }

    Ok(format!(
        "{}\n共新增 {} 个事件 hook。请重启 Claude Code / Antigravity IDE / Codex 生效。",
        msgs.join("\n"),
        total_added
    ))
}

fn process_uninstall_for_path(path: &PathBuf) -> Result<usize, String> {
    if !path.exists() {
        return Ok(0);
    }
    let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut root: Value = serde_json::from_str(&txt).map_err(|e| format!("{} 解析失败: {}", path.display(), e))?;
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
    std::fs::write(path, out).map_err(|e| e.to_string())?;
    Ok(removed)
}

/// 移除我们加的 hook 条目（按本地端点 URL 特征匹配）。
pub fn uninstall() -> Result<String, String> {
    let paths = settings_paths()?;
    let mut total_removed = 0;
    
    for path in paths {
        if let Ok(removed) = process_uninstall_for_path(&path) {
            total_removed += removed;
        }
    }

    Ok(format!(
        "共从各个配置文件中移除 {} 个 hook 条目。请重启 Claude Code / Antigravity IDE / Codex 生效。",
        total_removed
    ))
}
