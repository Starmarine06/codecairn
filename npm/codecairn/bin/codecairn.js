#!/usr/bin/env node
const { spawnSync } = require("child_process");
const { platform, arch } = process;
const key = `${platform}-${arch}`;
const exe = platform === "win32" ? "codecairn.exe" : "codecairn";

let bin;
try {
  bin = require.resolve(`@codecairn/${key}/bin/${exe}`);
} catch {
  // Try musl variant on Linux if glibc not found
  if (platform === "linux" && arch === "x64") {
    try {
      bin = require.resolve("@codecairn/linux-x64-musl/bin/codecairn");
    } catch {}
  }
  if (!bin) {
    console.error(`codecairn: no prebuilt binary for ${key}. Try: cargo install codecairn`);
    process.exit(1);
  }
}

const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(r.status ?? 1);