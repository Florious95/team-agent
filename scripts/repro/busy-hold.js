#!/usr/bin/env node
// //!
// //! purpose: busy-shell 夹具：holdMs 内不读 stdin（回车排队），之后只按 CR 落一行
// //! contract: 不打印 thinking/working；不 echo（setRawMode）；side 一行 = 一颗 C-m
// //! boundary: 只给 stall-resend 不重复齿用；不进产品运行时
// //! maturity: wired
const fs = require("fs");
const side = process.argv[2];
const holdMs = Number(process.argv[3] || 12000);
if (process.stdin.setRawMode) process.stdin.setRawMode(true);
process.stdin.pause();
process.stdout.write("busy-hold-ready\n");
setTimeout(() => {
  process.stdin.resume();
  process.stdin.setEncoding("utf8");
  let buf = "";
  process.stdin.on("data", (d) => {
    for (const ch of d) {
      if (ch === "\u0003") process.exit(0);
      if (ch === "\r") {
        fs.appendFileSync(side, buf + "\n");
        process.stdout.write("GOT\n");
        buf = "";
      } else if (ch !== "\n") {
        buf += ch;
      }
    }
  });
}, holdMs);
