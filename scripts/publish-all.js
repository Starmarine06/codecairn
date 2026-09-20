const { execSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const rootDir = path.resolve(__dirname, '..');
const mainPkgPath = path.join(rootDir, 'npm', 'codecairn', 'package.json');
const cargotomlPath = path.join(rootDir, 'Cargo.toml');
const platformsDir = path.join(rootDir, 'npm', 'platforms');

const bumpType = process.argv[2] || 'patch';
if (!['patch', 'minor', 'major'].includes(bumpType)) {
  console.error(`Usage: node scripts/publish-all.js [patch|minor|major]`);
  process.exit(1);
}

function bumpVersion(v) {
  const parts = v.split('.').map((n) => parseInt(n, 10) || 0);
  if (bumpType === 'major') {
    parts[0]++;
    parts[1] = 0;
    parts[2] = 0;
  } else if (bumpType === 'minor') {
    parts[1]++;
    parts[2] = 0;
  } else {
    parts[2]++;
  }
  return parts.join('.');
}

function readJson(p) {
  return JSON.parse(fs.readFileSync(p, 'utf8'));
}

function writeJson(p, obj) {
  fs.writeFileSync(p, JSON.stringify(obj, null, 2) + '\n');
}

const mainPkg = readJson(mainPkgPath);
const oldVersion = mainPkg.version;
const newVersion = bumpVersion(oldVersion);
console.log(`==> Bumping ${oldVersion} -> ${newVersion} (${bumpType})`);

mainPkg.version = newVersion;
for (const key of Object.keys(mainPkg.optionalDependencies || {})) {
  mainPkg.optionalDependencies[key] = newVersion;
}
writeJson(mainPkgPath, mainPkg);

let cargotoml = fs.readFileSync(cargotomlPath, 'utf8');
if (!cargotoml.includes(`version = "${oldVersion}"`)) {
  console.error(`Cargo.toml version does not match ${oldVersion}; refusing to bump.`);
  process.exit(1);
}
cargotoml = cargotoml.replace(`version = "${oldVersion}"`, `version = "${newVersion}"`);
fs.writeFileSync(cargotomlPath, cargotoml);

for (const dir of fs.readdirSync(platformsDir)) {
  const pkgPath = path.join(platformsDir, dir, 'package.json');
  if (!fs.existsSync(pkgPath)) continue;
  const pkg = readJson(pkgPath);
  pkg.version = newVersion;
  writeJson(pkgPath, pkg);
}

console.log('\n==> Building platform packages...');
execSync('node scripts/build.mjs build', { stdio: 'inherit', cwd: rootDir });

console.log('\n==> Publishing platform binary packages...');
const skipped = [];
for (const dir of fs.readdirSync(platformsDir)) {
  const pkgDir = path.join(platformsDir, dir);
  const binDir = path.join(pkgDir, 'bin');
  const binFiles = fs.existsSync(binDir)
    ? fs.readdirSync(binDir).filter((f) => f !== '.gitkeep')
    : [];
  if (binFiles.length === 0) {
    skipped.push(dir);
    console.log(`Skipping ${dir} (no binary compiled).`);
    continue;
  }
  console.log(`\nPublishing ${dir}...`);
  try {
    execSync('npm publish --access public', { stdio: 'inherit', cwd: pkgDir });
  } catch (err) {
    console.error(`Failed to publish ${dir}: ${err.message}`);
  }
}

if (skipped.length > 0) {
  console.warn(
    `\nWARNING: skipped ${skipped.join(', ')} (not built here). ` +
      `Main package optionalDependencies already point at ${newVersion} for them; ` +
      `publish them from a machine with those targets or via the tag-push CI workflow, ` +
      `otherwise installs on those platforms will fail.`
  );
}

console.log('\n==> Publishing main codecairn package...');
try {
  execSync('npm publish --access public', { stdio: 'inherit', cwd: path.join(rootDir, 'npm', 'codecairn') });
  console.log(`\n[SUCCESS] Published codecairn@${newVersion} to npm.`);
} catch (err) {
  console.error(`Failed to publish root package: ${err.message}`);
}

console.log('\nOptionally publish the crate too:');
console.log('  cargo publish');
console.log('And tag the release:');
console.log(`  git add -A && git commit -m "bump: v${newVersion}" && git tag v${newVersion} && git push origin master --tags`);