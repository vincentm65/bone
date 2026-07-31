import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import vm from 'node:vm';

const source = (await readFile(new URL('../bin/bone.js', import.meta.url), 'utf8'))
  .replace(/^#!.*\n/, '');

function runLauncher(platform, args) {
  const calls = [];
  const exit = Symbol('exit');
  const process = {
    platform,
    arch: 'x64',
    argv: ['node', 'bone.js', ...args],
    env: { EXISTING: 'kept' },
    exit(code) {
      throw { marker: exit, code };
    }
  };
  const require = (name) => {
    if (name === 'node:child_process') {
      return {
        spawnSync(command, commandArgs, options) {
          calls.push({ command, args: Array.from(commandArgs), options });
          return { status: 0 };
        }
      };
    }
    if (name === 'node:path') return path;
    throw new Error(`unexpected require: ${name}`);
  };
  require.resolve = (name) => `/packages/${name}`;

  let exitCode;
  try {
    vm.runInNewContext(source, {
      console: { log() {}, error() {} },
      process,
      require
    });
  } catch (error) {
    if (error?.marker !== exit) throw error;
    exitCode = error.code;
  }
  return { calls, exitCode };
}

test('Windows updates through npm before starting the locked native executable', () => {
  const result = runLauncher('win32', ['update', '--yes']);
  assert.equal(result.exitCode, 0);
  assert.equal(result.calls.length, 1);
  assert.equal(result.calls[0].command, 'npm.cmd');
  assert.deepEqual(result.calls[0].args, ['install', '-g', 'bone-agent@latest']);
});

test('normal launches identify npm as the installation source', () => {
  const result = runLauncher('linux', ['version', '--verbose']);
  assert.equal(result.exitCode, 0);
  assert.equal(result.calls.length, 1);
  assert.equal(result.calls[0].command, '/packages/bone-agent-linux-x64/bin/bone');
  assert.deepEqual(result.calls[0].args, ['version', '--verbose']);
  assert.equal(result.calls[0].options.env.EXISTING, 'kept');
  assert.equal(result.calls[0].options.env.BONE_INSTALL_KIND, 'npm');
});
