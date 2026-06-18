#!/usr/bin/env node
// Claude Code hook logger — U1 spike harness.
//
// Purpose: capture the REAL Claude Code hook event sequence + stdin payloads,
// so we can confirm the event->state mapping (KTD1) before building the app.
//
// It is configured as a hook command (see spike/README.md). On each hook fire,
// Claude Code runs:  node <path>/hook-logger.mjs <EventName>
// and pipes the hook JSON payload on stdin. We append one timestamped JSON
// line to spike/hook-events.log and exit fast (fire-and-forget; never block Claude).

import { appendFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const event = process.argv[2] ?? "unknown";
const here = dirname(fileURLToPath(import.meta.url));
const logPath = join(here, "hook-events.log");

let raw = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  raw += chunk;
});
process.stdin.on("end", () => {
  let payload;
  try {
    payload = JSON.parse(raw || "{}");
  } catch {
    payload = { _parseError: true, _raw: raw };
  }
  const record = {
    ts: new Date().toISOString(),
    event, // event name we passed on the command line
    hook_event_name: payload.hook_event_name, // event name Claude Code reports
    session_id: payload.session_id,
    cwd: payload.cwd,
    message: payload.message, // present on Notification — KEY for red/yellow split
    payload, // full raw payload for inspection
  };
  try {
    appendFileSync(logPath, JSON.stringify(record) + "\n");
  } catch {
    // Never block or fail Claude because of logging.
  }
  process.exit(0);
});

// Safety net: if stdin never closes, don't hang the hook.
setTimeout(() => process.exit(0), 800);
