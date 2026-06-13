#!/usr/bin/env node
/**
 * bone-rs - npm wrapper for the Rust binary
 * Finds the correct platform binary and executes it.
 */

const { spawnSync } = require('child_process');
const os = require('os');
const path = require('path');
const fs = require('fs');

// Resolve package directory from package.json location
let packageDir;
try {
  packageDir = path.dirname(require.resolve('../package.json'));
} catch {
  // Fallback: walk up from this file
  packageDir = __dirname;
  while (packageDir !== path.dirname(packageDir)) {
    if (fs.existsSync(path.join(packageDir, 'package.json'))) break;
    packageDir = path.dirname(packageDir);
  }
}

function getBinaryPath() {
  const plat = os.platform();
  const arch = os.arch();
  const ext = plat === 'win32' ? '.exe' : '';
  const key = `${plat}-${arch}`;

  const binPath = path.join(packageDir, 'bin', `bone-${key}${ext}`);
  if (fs.existsSync(binPath)) return binPath;

  // Fallback for arm64 on darwin (Apple Silicon) — try x64 Rosetta binary
  if (plat === 'darwin' && arch === 'arm64') {
    const x64Path = path.join(packageDir, 'bin', `bone-darwin-x64${ext}`);
    if (fs.existsSync(x64Path)) return x64Path;
  }

  return null;
}

const binPath = getBinaryPath();
if (!binPath) {
  console.error(
    `bone-rs: no binary found for ${os.platform()}-${os.arch()}. ` +
    `Supported: linux-x64, darwin-x64, darwin-arm64, win-x64`
  );
  process.exit(1);
}

const result = spawnSync(binPath, process.argv.slice(2), {
  stdio: 'inherit',
  cwd: process.cwd(),
});

process.exit(result.status ?? 1);
