#!/usr/bin/env node
// //!
// //! purpose: 不消费 TUI 夹具（非 copy-mode）：可按 thinkMs 丢 Enter，也可先丢 N 颗再提交
// //! contract: 每颗 CR/LF 记 enter.log；不打印 thinking/working/esc to interrupt（避免 busy=已消费）
// //! boundary: 只给 stall-resend 复现装置用；不进产品运行时
// //! maturity: wired
const fs = require("fs");
const side = process.argv[2];
const enterLog = process.argv[3];
const thinkMs = Number(process.argv[4] || 0);
const dropFirst = Number(process.argv[5] || 0);
const started = Date.now();
let buf = "";
let dropped = 0;
if (process.stdin.setRawMode) process.stdin.setRawMode(true);
process.stdin.resume();
process.stdin.setEncoding("utf8");
process.stdout.write("tui-ready composer>\n");
process.stdin.on("data", (d) => {
  for (const ch of d) {
    if (ch === "\r") {
      const now = Date.now();
      fs.appendFileSync(enterLog, String(now) + "\n");
      if (dropped < dropFirst) {
        dropped += 1;
        continue;
      }
      if (now - started < thinkMs) {
        continue;
      }
      fs.appendFileSync(side, buf + "\n");
      process.stdout.write("\nGOT\n");
      buf = "";
    } else if (ch === "\n") {
      buf += ch;
    } else if (ch === "\u0003") {
      process.exit(0);
    } else {
      buf += ch;
      process.stdout.write(ch);
    }
  }
});
