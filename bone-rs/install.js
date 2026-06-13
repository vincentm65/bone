#!/usr/bin/env node
/**
 * bone-rs install script
 * Extracts bundled Lua files to ~/.bone-rust/ (persists across npm updates).
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

const packageDir = path.join(__dirname, '..');
const luaDir = path.join(packageDir, 'defaults', 'lua');
const boneDir = path.join(os.homedir(), '.bone-rust');
const luaTarget = path.join(boneDir, 'lua');

// Create ~/.bone-rust/ if it doesn't exist
if (!fs.existsSync(boneDir)) {
  fs.mkdirSync(boneDir, { recursive: true });
}

// Copy bundled Lua files to ~/.bone-rust/lua/ (only if target doesn't exist)
function copyRecursive(src, dest) {
  const stat = fs.statSync(src);
  if (stat.isDirectory()) {
    if (!fs.existsSync(dest)) {
      fs.mkdirSync(dest, { recursive: true });
    }
    for (const entry of fs.readdirSync(src)) {
      copyRecursive(path.join(src, entry), path.join(dest, entry));
    }
  } else {
    if (!fs.existsSync(dest)) {
      fs.copyFileSync(src, dest);
    }
  }
}

if (fs.existsSync(luaDir)) {
  copyRecursive(luaDir, luaTarget);
  console.log('✓ Lua files installed to ~/.bone-rust/lua/');
} else {
  console.log('⚠ No bundled Lua files found');
}
