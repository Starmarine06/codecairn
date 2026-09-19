import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const npmRoot = path.join(repoRoot, 'npm');
const shell = process.platform === 'win32';

const SUPPORTED = {
  'x86_64-pc-windows-msvc': 'win32-x64',
  'aarch64-pc-windows-msvc': 'win32-arm64',
  'x86_64-apple-darwin': 'darwin-x64',
  'aarch64-apple-darwin': 'darwin-arm64',
  'x86_64-unknown-linux-gnu': 'linux-x64',
  'aarch64-unknown-linux-gnu': 'linux-arm64',
  'x86_64-unknown-linux-musl': 'linux-x64-musl',
};

function hostTriple() {
  switch (`${process.platform}/${process.arch}`) {
    case 'win32/x64':
      return 'x86_64-pc-windows-msvc';
    case 'win32/arm64':
      return 'aarch64-pc-windows-msvc';
    case 'darwin/arm64':
      return 'aarch64-apple-darwin';
    case 'darwin/x64':
      return 'x86_64-apple-darwin';
    case 'linux/arm64':
      return 'aarch64-unknown-linux-gnu';
    case 'linux/x64':
      return 'x86_64-unknown-linux-gnu';
    default:
      return null;
  }
}

function npmPlatformForHost() {
  const arch = process.arch === 'x64' ? 'x64' : process.arch === 'arm64' ? 'arm64' : null;
  const osName = process.platform === 'win32' ? 'win32' : process.platform === 'darwin' ? 'darwin' : process.platform === 'linux' ? 'linux' : null;
  if (!osName || !arch || !(osName === 'linux' ? ['x64', 'arm64'] : ['x64', 'arm64']).includes(arch)) return null;
  return `${osName}-${arch}`;
}

function shellQuote(s) {
  const safe = /^[A-Za-z0-9_\\/:.@-]+$/;
  if (safe.test(s)) return s;
  return shell ? `"${s.replace(/"/g, '""')}"` : `'${s.replace(/'/g, `'\\''`)}'`;
}

function cmdline(cmd, args) {
  return [cmd, ...args].map(shellQuote).join(' ');
}

function run(cmd, args, opts = {}) {
  if (shell) {
    execFileSync(cmdline(cmd, args), { shell: true, stdio: 'inherit', ...opts });
  } else {
    execFileSync(cmd, args, { stdio: 'inherit', ...opts });
  }
}

function capture(cmd, args, opts = {}) {
  const out = shell
    ? execFileSync(cmdline(cmd, args), { shell: true, encoding: 'utf8', ...opts })
    : execFileSync(cmd, args, { encoding: 'utf8', ...opts });
  return out.trim();
}

function installedTargets() {
  try {
    return capture('rustup', ['target', 'list', '--installed'])
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean);
  } catch {
    return [];
  }
}

function buildTargets() {
  const host = hostTriple();
  const set = new Set(
    installedTargets().filter((t) => SUPPORTED[t]),
  );
  if (host) set.add(host);
  return [...set];
}

function rmDir(p) {
  fs.rmSync(p, { recursive: true, force: true });
}

function clean() {
  console.log(`[clean] agent ${repoRoot}`);
  rmDir(path.join(repoRoot, 'target'));
  for (const name of Object.values(SUPPORTED)) {
    rmDir(path.join(npmRoot, 'platforms', name, 'bin'));
  }
  for (const file of fs.readdirSync(npmRoot, { recursive: true })) {
    if (typeof file === 'string' && file.endsWith('.tgz')) {
      fs.rmSync(path.join(npmRoot, file), { force: true });
    }
  }
  fs.rmSync(path.join(repoRoot, 'Cargo.lock'), { force: true });
  for (const entry of fs.readdirSync(os.tmpdir(), { withFileTypes: true })) {
    if (entry.isDirectory() && entry.name.startsWith('codecairn-smoke-')) {
      rmDir(path.join(os.tmpdir(), entry.name));
    }
  }
}

function build() {
  const targets = buildTargets();
  if (targets.length === 0) {
    throw new Error('no supported rust target available; install one via `rustup target add <triple>`');
  }
  console.log(`[build] targets: ${targets.join(', ')}`);

  for (const triple of targets) {
    run('cargo', ['build', '--release', '--target', triple], { cwd: repoRoot });
  }
  for (const triple of targets) {
    run('cargo', ['run', '--quiet', '--release', '--bin', 'gen-platform-pkgs', '--', triple], { cwd: repoRoot });
  }

  const packed = [];
  const dirs = [['codecairn', path.join(npmRoot, 'codecairn')], ...targets.map((t) => [SUPPORTED[t], path.join(npmRoot, 'platforms', SUPPORTED[t])])];
  for (const [name, pkgDir] of dirs) {
    const out = JSON.parse(capture('npm', ['pack', '--json'], { cwd: pkgDir }));
    const match = out.find((f) => f.name.endsWith(name));
    if (!match) throw new Error(`npm pack produced no entry for ${name}`);
    packed.push({ name, tgz: path.join(pkgDir, match.filename) });
    console.log(`[build] packed ${name}`);
  }
  return packed;
}

function smoke() {
  const hostPkg = npmPlatformForHost();
  if (!hostPkg) {
    console.warn('[smoke] host platform not supported, skipping');
    return;
  }
  const dir = path.join(os.tmpdir(), `codecairn-smoke-${process.pid}`);
  rmDir(dir);
  fs.mkdirSync(dir, { recursive: true });

  const mainTgz = path.join(npmRoot, 'codecairn');
  const main = fs.readdirSync(mainTgz).find((f) => f.startsWith('codecairn-') && f.endsWith('.tgz'));
  const platDir = path.join(npmRoot, 'platforms', hostPkg);
  const plat = fs.readdirSync(platDir).find((f) => f.startsWith(`codecairn-${hostPkg}-`) && f.endsWith('.tgz'));
  if (!main || !plat) {
    rmDir(dir);
    throw new Error('missing packed tarballs; run `build` first');
  }

  try {
    capture('npm', ['init', '-y'], { cwd: dir });
    run('npm', ['install', '--no-audit', '--no-fund', path.join(mainTgz, main), path.join(platDir, plat)], { cwd: dir });

    console.log('--- codecairn --version ---');
    run('npx', ['codecairn', '--version'], { cwd: dir });
    console.log('--- codecairn <repoRoot> (truncated) ---');
    const map = capture('npx', ['codecairn', repoRoot], { cwd: dir });
    console.log(map.slice(0, 8000));
  } finally {
    rmDir(dir);
  }
}

const [, , cmd = 'rebuild'] = process.argv;
switch (cmd) {
  case 'clean':
    clean();
    break;
  case 'build':
    build();
    break;
  case 'smoke':
    smoke();
    break;
  case 'rebuild':
    clean();
    build();
    smoke();
    break;
  default:
    console.error(`usage: node scripts/build.mjs [clean|build|smoke|rebuild]`);
    process.exit(1);
}