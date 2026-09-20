const fs = require('fs');
const path = require('path');

const rootDir = path.resolve(__dirname, '..');

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

function copyBinary(triple) {
  const platform = SUPPORTED[triple];
  if (!platform) throw new Error(`Unsupported target triple: ${triple}`);

  const exeName = triple.includes('windows') ? 'codecairn.exe' : 'codecairn';
  const srcBin = path.join(rootDir, 'target', triple, 'release', exeName);
  const dstDir = path.join(rootDir, 'npm', 'platforms', platform, 'bin');
  const dstBin = path.join(dstDir, exeName);

  if (!fs.existsSync(srcBin)) return false;

  fs.mkdirSync(dstDir, { recursive: true });
  fs.copyFileSync(srcBin, dstBin);
  if (process.platform !== 'win32') {
    fs.chmodSync(dstBin, 0o755);
  }
  console.log(`[OK] ${srcBin} -> ${dstBin}`);
  return true;
}

if (require.main === module) {
  const triple = process.argv[2] || hostTriple();
  if (!triple) {
    console.error('Cannot detect host triple; pass a target triple explicitly.');
    process.exit(1);
  }
  if (!copyBinary(triple)) {
    console.warn(`[WARN] Release binary not found for ${triple}. Run 'npm run build' first.`);
  }
}

module.exports = { SUPPORTED, hostTriple, copyBinary };