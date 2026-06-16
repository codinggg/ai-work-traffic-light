// 极简静态服务器，仅用于 `tauri dev`：把 src/ 通过 http 提供给 webview。
// 原因：dev 模式 (cargo run --no-default-features) 关闭了 custom-protocol，
// 前端不内嵌，需要从 devUrl 加载。打包 (tauri build) 不用它。无第三方依赖。
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname } from "node:path";

const ROOT = new URL("../src/", import.meta.url);
const PORT = 1420;
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
};

const server = createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, "http://localhost").pathname);
  const rel = path.replace(/^\/+/, "") || "index.html";
  try {
    const body = await readFile(new URL(rel, ROOT));
    res.setHeader("Content-Type", TYPES[extname(rel)] ?? "application/octet-stream");
    res.end(body);
  } catch {
    res.statusCode = 404;
    res.end("not found");
  }
});

// 端口被占用时(上次 dev 残留的服务器还活着)：复用它，正常退出，让 tauri dev 继续。
server.on("error", (err) => {
  if (err && err.code === "EADDRINUSE") {
    console.log(`端口 ${PORT} 已被占用，复用已在运行的 dev server。`);
    process.exit(0);
  }
  console.error(err);
  process.exit(1);
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`dev static server: http://localhost:${PORT}`);
});
