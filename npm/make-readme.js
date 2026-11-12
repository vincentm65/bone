#!/usr/bin/env node

// Writes npm/bone-agent/README.md from the repository's README.md, so the
// npm page shows the same text. The package is published from a subfolder,
// where npm cannot resolve the README's relative links and images, so each
// one is pointed at GitHub.
//
//   node npm/make-readme.js [ref]    ref: a tag, branch or commit for the
//                                    links (default: $GITHUB_SHA, then main)

const fs = require('node:fs');
const path = require('node:path');

const repo = 'vincentm65/bone';
const ref = process.argv[2] || process.env.GITHUB_SHA || 'main';
const root = path.resolve(__dirname, '..');

const absolute = /^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i;
const image = /\.(?:png|jpe?g|gif|webp|svg)$/i;

function url(target) {
  if (absolute.test(target)) return target;
  const [file, hash = ''] = target.split(/(?=#)/);
  const clean = file.replace(/^\.?\//, '');
  if (!fs.existsSync(path.join(root, clean))) {
    throw new Error(`README.md links to ${clean}, which does not exist`);
  }
  return image.test(clean)
    ? `https://raw.githubusercontent.com/${repo}/${ref}/${clean}`
    : `https://github.com/${repo}/blob/${ref}/${clean}${hash}`;
}

const readme = fs
  .readFileSync(path.join(root, 'README.md'), 'utf8')
  // Markdown links and images: [text](target) and ![alt](target)
  .replace(/(\]\()([^)\s]+)(\))/g, (_, open, target, close) => open + url(target) + close)
  // HTML: src="target" and href="target"
  .replace(/\b(src|href)="([^"]+)"/g, (_, attr, target) => `${attr}="${url(target)}"`);

const out = path.join(root, 'npm', 'bone-agent', 'README.md');
fs.writeFileSync(out, readme);
console.log(`wrote ${path.relative(root, out)} (links at ${ref})`);
