#!/usr/bin/env node

const { spawnSync } = require('node:child_process');
const path = require('node:path');

const packages = {
  'linux-x64': 'bone-agent-linux-x64',
  'linux-arm64': 'bone-agent-linux-arm64',
  'darwin-x64': 'bone-agent-darwin-x64',
  'darwin-arm64': 'bone-agent-darwin-arm64',
  'win32-x64': 'bone-agent-windows-x64'
};

const key = `${process.platform}-${process.arch}`;
const packageName = packages[key];
const args = process.argv.slice(2);

if (!packageName) {
  console.error(`bone-agent does not support ${key}`);
  process.exit(1);
}

// The native binary has no self-update, so handle `bone update` here through
// npm. This also avoids Windows' lock on the running bone.exe.
if (args[0] === 'update') {
  console.log('Updating bone through npm...');
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const update = spawnSync(npm, ['install', '-g', 'bone-agent@latest'], {
    stdio: 'inherit',
    shell: process.platform === 'win32'
  });
  if (update.error) {
    console.error(update.error.message);
    process.exit(1);
  }
  process.exit(update.status ?? 1);
}

let packageJson;
try {
  packageJson = require.resolve(`${packageName}/package.json`);
} catch {
  console.error(`Missing native package ${packageName}. Try reinstalling bone-agent.`);
  process.exit(1);
}

const exe = process.platform === 'win32' ? 'bone.exe' : 'bone';
const bin = path.join(path.dirname(packageJson), 'bin', exe);
const result = spawnSync(bin, args, {
  stdio: 'inherit',
  env: { ...process.env, BONE_INSTALL_KIND: 'npm' }
});

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

process.exit(result.status ?? 1);
