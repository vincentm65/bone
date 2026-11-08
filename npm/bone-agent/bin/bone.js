#!/usr/bin/env node

const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
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

// The desktop GUI is its own binary, so the TUI one needs no window system
// libraries (and still starts on a server without a desktop).
const nativeDir = path.dirname(packageJson);
const guiExe = process.platform === 'win32' ? 'bone-gui.exe' : 'bone-gui';
const guiBin = path.join(nativeDir, 'bin', guiExe);
const gui = args.includes('--gui') || args[0] === 'desktop';
if (gui && !fs.existsSync(guiBin)) {
  console.error(`This bone-agent install has no desktop GUI (${guiBin}). Try reinstalling bone-agent.`);
  process.exit(1);
}

// `bone desktop [--remove]`: a Start menu / Applications / app menu entry
// for the desktop app (see desktop.js).
if (args[0] === 'desktop') {
  const desktop = require('./desktop');
  const remove = args.includes('--remove');
  try {
    console.log(desktop.run(guiBin, path.join(nativeDir, 'share'), require('../package.json').version, remove));
  } catch (e) {
    console.error(`bone desktop: ${e.message}`);
    process.exit(1);
  }
  process.exit(0);
}

// The desktop app runs on its own: the terminal is free again at once, and
// closing it leaves the window open.
if (gui) {
  const child = spawn(guiBin, args, {
    detached: true,
    stdio: 'ignore',
    env: { ...process.env, BONE_INSTALL_KIND: 'npm' }
  });
  child.on('error', (e) => {
    console.error(e.message);
    process.exit(1);
  });
  child.on('spawn', () => {
    child.unref();
    if (!require('./desktop').installed()) {
      console.log(`Opening the desktop app. Run \`bone desktop\` to add it to ${require('./desktop').where()}.`);
    }
    process.exit(0);
  });
  return;
}

const exe = process.platform === 'win32' ? 'bone.exe' : 'bone';
const bin = path.join(nativeDir, 'bin', exe);
const result = spawnSync(bin, args, {
  stdio: 'inherit',
  env: { ...process.env, BONE_INSTALL_KIND: 'npm' }
});

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

process.exit(result.status ?? 1);
