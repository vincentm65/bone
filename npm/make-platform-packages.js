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

const version = process.argv[2] || process.env.npm_package_version;
if (!version) {
  console.error('usage: node npm/make-platform-packages.js <version>');
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
  const source = path.join(root, 'target', triple, 'release', exe);
  const dest = path.join(binDir, exe);

  if (!fs.existsSync(source)) {
    console.error(`missing binary: ${source}`);
    process.exit(1);
  }

  fs.mkdirSync(binDir, { recursive: true });
  fs.copyFileSync(source, dest);
  if (os !== 'win32') fs.chmodSync(dest, 0o755);

  // Bundle web UI assets so `bone web` works when installed via npm.
  const webuiSrc = path.join(root, 'webui');
  const webuiDst = path.join(dir, 'webui');
  if (fs.existsSync(webuiSrc)) {
    fs.cpSync(webuiSrc, webuiDst, { recursive: true });
  } else {
    console.error('warning: webui/ not found, `bone web` may not work');
  }

  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
    name,
    version,
    description: `Native binary for bone-agent on ${os} ${cpu}`,
    bin: { bone: `bin/${exe}` },
    files: ['bin', 'webui'],
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
