# U1 Spike — 验证 Claude Code hook 真实行为

目标：在写 app 之前，用真实数据确认 hook 的事件序列与字段，尤其：
- `Notification` 到底在什么时候触发，`message` 文本能否区分"需用户处理（请求权限/确认）"与"空闲等待"——这决定红/黄是否会误报（计划最高风险项）。
- `session_id` / `cwd` 是否在负载里（用于区分会话、推导项目名）。
- `SessionStart` 的时机；终端 vs VSCode 扩展是否一致。

## 运行步骤

1. 确认 Node 可用（已确认：v23）。

2. 启用 hooks。**二选一：**

   **A. 项目级（推荐，只影响本仓库）** —— 把下面内容写进本仓库的 `.claude/settings.json`（在仓库根目录运行 Claude Code 时 `cwd` 即根目录，故用相对路径）：

   ```json
   {
     "hooks": {
       "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs UserPromptSubmit" }] }],
       "PreToolUse":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs PreToolUse" }] }],
       "PostToolUse":      [{ "matcher": "", "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs PostToolUse" }] }],
       "Notification":     [{ "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs Notification" }] }],
       "Stop":             [{ "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs Stop" }] }],
       "SessionStart":     [{ "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs SessionStart" }] }],
       "SessionEnd":       [{ "hooks": [{ "type": "command", "command": "node spike/hook-logger.mjs SessionEnd" }] }]
     }
   }
   ```

   **B. 全局（影响所有项目）** —— 写进 `~/.claude/settings.json`，但命令改用本脚本的**绝对路径**，例如：
   `node "e:/mycode/work/cluster/ai-work-traffic-light/spike/hook-logger.mjs" Notification`

3. **重启 Claude Code**（hooks 在会话启动时加载，运行中改配置不生效）。

4. 在本仓库里跑一次真实任务，刻意制造：
   - 一次**需要权限确认**的操作（例如让 Claude 执行一条需要授权的命令），看 `Notification`。
   - 一次**长时间不输入的空闲**（让会话挂着 1–2 分钟），看是否有"空闲等待"类 `Notification`。
   - 终端里跑一遍，再在 VSCode 扩展里跑一遍。

5. 查看 `spike/hook-events.log`，把观察填进 `docs/hook-spike-findings.md`。

## 关注点

- 每条 `Notification` 的 `message` 长什么样？权限请求 vs 空闲等待能否区分？
- `session_id`、`cwd` 是否都在？多开两个会话时 `session_id` 是否不同？
- 授权放行后，下一个事件是 `PreToolUse`/`PostToolUse` 吗（用于红→绿恢复）？
- 终端与 VSCode 扩展的事件/字段是否一致？

## 清理

验证完，从 settings 里移除上面的 hooks（或删掉 `.claude/settings.json`），并可删除 `spike/hook-events.log`。
