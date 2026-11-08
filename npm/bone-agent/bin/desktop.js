// `bone desktop`: put the desktop app where people look for apps, pointing
// at the bone-gui binary in this npm install. `bone update` replaces that
// binary in place, so the shortcut keeps opening the current version.
//
//   Windows  Start menu and desktop shortcuts (Bone.lnk)
//   macOS    ~/Applications/Bone.app (Spotlight, Launchpad, the Dock)
//   Linux    ~/.local/share/applications/bone-agent.desktop (the app menu)
//
// `bone desktop --remove` takes them away again.

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const NAME = 'Bone';

function home(...parts) {
  return path.join(os.homedir(), ...parts);
}

// Where the shortcut goes on this system (what `installed` looks for).
function places() {
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA || home('AppData', 'Roaming');
    return [path.join(appData, 'Microsoft', 'Windows', 'Start Menu', 'Programs', `${NAME}.lnk`)];
  }
  if (process.platform === 'darwin') return [home('Applications', `${NAME}.app`)];
  const data = process.env.XDG_DATA_HOME || home('.local', 'share');
  return [path.join(data, 'applications', 'bone-agent.desktop')];
}

function installed() {
  return places().some((p) => fs.existsSync(p));
}

// PowerShell for the Windows shortcuts; paths come in through the
// environment, so nothing needs quoting.
const WINDOWS_SCRIPT = `
$shell = New-Object -ComObject WScript.Shell
$desktop = [Environment]::GetFolderPath('Desktop')
foreach ($path in @($env:BONE_LNK_MENU, (Join-Path $desktop '${NAME}.lnk'))) {
  if ($env:BONE_LNK_REMOVE -eq '1') {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    continue
  }
  $link = $shell.CreateShortcut($path)
  $link.TargetPath = $env:BONE_LNK_TARGET
  $link.Arguments = '--gui'
  $link.WorkingDirectory = $env:USERPROFILE
  $link.IconLocation = "$($env:BONE_LNK_TARGET),0"
  $link.Description = 'Open the ${NAME} desktop app'
  $link.Save()
}
`;

function windows(gui, remove) {
  const r = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', WINDOWS_SCRIPT], {
    stdio: ['ignore', 'inherit', 'inherit'],
    env: { ...process.env, BONE_LNK_MENU: places()[0], BONE_LNK_TARGET: gui || '', BONE_LNK_REMOVE: remove ? '1' : '0' },
  });
  if (r.error) throw r.error;
  if (r.status !== 0) throw new Error(`PowerShell could not ${remove ? 'remove' : 'make'} the shortcuts`);
  return remove ? 'Removed the Start menu and desktop shortcuts.' : 'Added Bone to the Start menu and the desktop.';
}

function xmlEscape(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// A single-quoted shell word.
function shQuote(s) {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

function mac(gui, share, version, remove) {
  const app = places()[0];
  fs.rmSync(app, { recursive: true, force: true });
  if (remove) return `Removed ${app}.`;
  const contents = path.join(app, 'Contents');
  fs.mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
  fs.mkdirSync(path.join(contents, 'Resources'), { recursive: true });
  const icon = path.join(share, 'icon.icns');
  if (fs.existsSync(icon)) fs.copyFileSync(icon, path.join(contents, 'Resources', 'bone.icns'));
  fs.writeFileSync(path.join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleDisplayName</key><string>${NAME}</string>
  <key>CFBundleIdentifier</key><string>com.vincentm65.bone</string>
  <key>CFBundleExecutable</key><string>bone</string>
  <key>CFBundleIconFile</key><string>bone</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${xmlEscape(version)}</string>
  <key>CFBundleVersion</key><string>${xmlEscape(version)}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
`);
  // An app opened from Finder gets a bare PATH; the agent's tools and
  // `bone update` (npm) need the one this terminal has.
  const launcher = path.join(contents, 'MacOS', 'bone');
  fs.writeFileSync(launcher, `#!/bin/sh
export PATH=${shQuote(process.env.PATH || '/usr/bin:/bin')}
cd "$HOME"
exec ${shQuote(gui)} --gui "$@"
`);
  fs.chmodSync(launcher, 0o755);
  return `Added ${app}: open Bone from Spotlight, Launchpad or Applications.`;
}

// A value for a .desktop file's Exec line: quoted, with its reserved
// characters escaped.
function execQuote(s) {
  return `"${s.replace(/[\\"`$]/g, (c) => `\\${c}`).replace(/%/g, '%%')}"`;
}

function linux(gui, share, remove) {
  const entry = places()[0];
  const data = path.dirname(path.dirname(entry));
  const iconFile = path.join(data, 'icons', 'hicolor', '512x512', 'apps', 'bone-agent.png');
  if (remove) {
    fs.rmSync(entry, { force: true });
    fs.rmSync(iconFile, { force: true });
    return `Removed ${entry}.`;
  }
  const icon = path.join(share, 'icon-512.png');
  if (fs.existsSync(icon)) {
    fs.mkdirSync(path.dirname(iconFile), { recursive: true });
    fs.copyFileSync(icon, iconFile);
  }
  fs.mkdirSync(path.dirname(entry), { recursive: true });
  // The app menu may start it without the PATH this terminal has (nvm,
  // ~/.local/bin); the agent's tools and `bone update` need it.
  const exec = ['env', execQuote(`PATH=${process.env.PATH || '/usr/bin:/bin'}`), execQuote(gui), '--gui'].join(' ');
  fs.writeFileSync(entry, `[Desktop Entry]
Type=Application
Name=${NAME}
Comment=AI coding agent
Exec=${exec}
Icon=${fs.existsSync(iconFile) ? 'bone-agent' : 'utilities-terminal'}
Terminal=false
Categories=Development;
StartupWMClass=bone
`);
  spawnSync('update-desktop-database', [path.dirname(entry)], { stdio: 'ignore' });
  return `Added Bone to your app menu (${entry}).`;
}

// Make (or with `remove`, take away) the shortcuts. `gui`: the bone-gui
// binary; `share`: the platform package's share directory.
function run(gui, share, version, remove) {
  if (process.platform === 'win32') return windows(gui, remove);
  if (process.platform === 'darwin') return mac(gui, share, version, remove);
  return linux(gui, share, remove);
}

// Where the hint after `bone --gui` says the app can be found.
function where() {
  if (process.platform === 'win32') return 'the Start menu and the desktop';
  if (process.platform === 'darwin') return 'Applications, Spotlight and Launchpad';
  return 'your app menu';
}

module.exports = { run, installed, where };
