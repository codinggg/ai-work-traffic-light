// 生成应用/托盘图标源图 app-icon.png（512x512，RGBA）。
// 设计：深色底 + 红/黄/绿三个灯点（呼应红绿灯主题）。
// 之后用 `pnpm tauri icon app-icon.png` 生成各尺寸 .png/.ico/.icns。
// 无第三方依赖，仅用 Node 内置 zlib 手工拼装 PNG。
import { writeFileSync } from "node:fs";
import { deflateSync, crc32 } from "node:zlib";

const S = 512;
const rgba = Buffer.alloc(S * S * 4);

const bg = [27, 31, 36]; // #1b1f24
const dots = [
  { cy: 138, col: [248, 81, 73] }, // 红
  { cy: 256, col: [210, 153, 34] }, // 黄
  { cy: 374, col: [46, 160, 67] }, // 绿
];
const cx = 256;
const r = 52;

for (let y = 0; y < S; y++) {
  for (let x = 0; x < S; x++) {
    let [cr, cg, cb] = bg;
    let a = 255;
    for (const d of dots) {
      const dx = x - cx;
      const dy = y - d.cy;
      const dist2 = dx * dx + dy * dy;
      if (dist2 <= r * r) {
        const gloss = Math.max(0, 1 - Math.sqrt(dist2) / r) * 45;
        cr = Math.min(255, d.col[0] + gloss);
        cg = Math.min(255, d.col[1] + gloss);
        cb = Math.min(255, d.col[2] + gloss);
      }
    }
    const i = (y * S + x) * 4;
    rgba[i] = cr;
    rgba[i + 1] = cg;
    rgba[i + 2] = cb;
    rgba[i + 3] = a;
  }
}

// 加 PNG 过滤字节（每行前置 0）
const stride = S * 4;
const raw = Buffer.alloc(S * (stride + 1));
for (let y = 0; y < S; y++) {
  raw[y * (stride + 1)] = 0;
  rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
}

const idat = deflateSync(raw);

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0, 0);
  return Buffer.concat([len, body, crc]);
}

const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0);
ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type RGBA
const png = Buffer.concat([
  sig,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

writeFileSync("app-icon.png", png);
console.log(`wrote app-icon.png (${png.length} bytes)`);
