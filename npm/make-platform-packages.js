#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');

const targets = [
  ['linux', 'x64', 'x86_64-unknown-linux-gnu', 'bone', 'linux-x64'],
  ['linux', 'arm64', 'aarch64-unknown-linux-gnu', 'bone', 'linux-arm64'],
  ['darwin', 'x64', 'x86_64-apple-darwin', 'bone', 'darwin-x64'],
  ['darwin', 'arm64', 'aarch64-apple-darwin', 'bone', 'darwin-arm64'],
  ['win32', 'x64', 'x86_64-pc-windows-msvc', 'bone.exe', 'windows-x64']
];

const host = process.argv.includes('--host');
const version = process.argv.slice(2).find((a) => !a.startsWith('--')) || process.env.npm_package_version;
if (!version) {
  console.error('usage: node npm/make-platform-packages.js <version> [--host]  (--host: only this machine, from target/release)');
  process.exit(1);
}

const root = path.resolve(__dirname, '..');
const dist = path.join(root, 'npm', 'dist');
fs.rmSync(dist, { recursive: true, force: true });
fs.mkdirSync(dist, { recursive: true });

for (const [os, cpu, triple, exe, packageTarget] of targets) {
  const name = `bone-agent-${packageTarget}`;
  const dir = path.join(dist, name);
  const binDir = path.join(dir, 'bin');
  if (host && !(os === process.platform && cpu === process.arch)) continue;
  const source = host
    ? path.join(root, 'target', 'release', exe)
    : path.join(root, 'target', triple, 'release', exe);
  // The desktop GUI's binary (`bone --gui`), beside the TUI one.
  const guiExe = exe.replace(/^bone/, 'bone-gui');
  const guiSource = path.join(path.dirname(source), guiExe);

  // A local (`--host`) package may have no GUI build; a release has both.
  const binaries = [[source, exe]];
  if (!host || fs.existsSync(guiSource)) binaries.push([guiSource, guiExe]);
  for (const [file] of binaries) {
    if (!fs.existsSync(file)) {
      console.error(`missing binary: ${file}`);
      process.exit(1);
    }
  }

  fs.mkdirSync(binDir, { recursive: true });
  for (const [from, file] of binaries) {
    const dest = path.join(binDir, file);
    fs.copyFileSync(from, dest);
    if (os !== 'win32') fs.chmodSync(dest, 0o755);
  }

  // The app's icon for the shortcut `bone desktop` makes: Bone.app's on
  // macOS, the app menu's on Linux (Windows takes it from bone-gui.exe).
  const icon = { darwin: 'icon.icns', linux: 'icon-512.png' }[os];
  if (icon) {
    const shareDir = path.join(dir, 'share');
    fs.mkdirSync(shareDir, { recursive: true });
    fs.copyFileSync(path.join(root, 'crates', 'bone-gui', 'assets', icon), path.join(shareDir, icon));
  }

  // The npm page of a platform package: point at the package to install.
  fs.writeFileSync(path.join(dir, 'README.md'), [
    `# ${name}`,
    '',
    `The native bone binary for ${os} ${cpu}. It is installed for you by`,
    '[bone-agent](https://www.npmjs.com/package/bone-agent); install that instead:',
    '',
    '```sh',
    'npm install -g bone-agent',
    '```',
    ''
  ].join('\n'));

  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
    name,
    version,
    description: `Native Bone binary for bone-agent on ${os} ${cpu}`,
    bin: { bone: `bin/${exe}` },
    files: ['bin', 'share'],
    os: [os],
    cpu: [cpu],
    engines: { node: '>=18' },
    author: 'Vincent Miranda <vincentmiranda65@gmail.com>',
    license: 'MIT',
    homepage: 'https://github.com/vincentm65/bone',
    repository: {
      type: 'git',
      url: 'git+https://github.com/vincentm65/bone.git'
    },
    bugs: {
      url: 'https://github.com/vincentm65/bone/issues'
    }
  }, null, 2) + '\n');
}
