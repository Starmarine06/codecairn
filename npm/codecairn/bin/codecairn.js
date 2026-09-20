#!/usr/bin/env node
const { spawnSync } = require("child_process");
const { platform, arch } = process;
const exe = platform === "win32" ? "codecairn.exe" : "codecairn";

function isMusl() {
  if (platform !== "linux") return false;
  try {
    const header = process.report.getReport().header;
    return !header.glibcVersionRuntime && !header.glibcVersionCompiler;
  } catch {
    return true;
  }
}

let key = `${platform}-${arch}`;
if (isMusl() && arch === "x64") {
  key = "linux-x64-musl";
}

let bin;
try {
  bin = require.resolve(`@codecairn/${key}/bin/${exe}`);
} catch {
  console.error(`codecairn: no prebuilt binary for ${key}. Try: cargo install codecairn`);
  process.exit(1);
}

const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(r.status ?? 1);