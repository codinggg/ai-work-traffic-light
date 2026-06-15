# U1 Findings — Claude Code hook 行为实测

> 状态：**待填写**。按 `spike/README.md` 跑完后，把观察结果填进下面各节，并据此确认/修正计划的 KTD1 映射。

## 1. 观察到的事件序列

跑一个"提交 prompt → Claude 工作 → 请求权限 → 授权 → 完成"的典型回合，按时间记录事件名：

```
(粘贴 spike/hook-events.log 的关键序列)
```

## 2. 各事件的负载字段

| 事件 | 有 session_id? | 有 cwd? | hook_event_name 值 | 其它关键字段 |
|---|---|---|---|---|
| UserPromptSubmit | | | | |
| PreToolUse | | | | |
| PostToolUse | | | | |
| Notification | | | | message: |
| Stop | | | | |
| SessionStart | | | | |
| SessionEnd | | | | |

## 3. Notification 子类型（最关键）

- 请求权限/需用户处理时，`message` = `__________`
- 空闲等待时，是否触发 Notification？`message` = `__________`
- **判别规则结论**：如何从负载区分"需处理→红"与"空闲等待→黄"？

## 4. 红→绿恢复

- 授权放行后，下一个事件是？（期望 PreToolUse/PostToolUse）`__________`
- 用户拒绝后呢？`__________`

## 5. 终端 vs VSCode 扩展

- 事件集合是否一致？`__________`
- 字段是否一致？`__________`
- 差异：`__________`

## 6. 对计划的影响

- KTD1 映射：确认 / 需修正为：`__________`
- 会话标识用 `cwd` 末段是否可行？`__________`
- 其它发现：`__________`
