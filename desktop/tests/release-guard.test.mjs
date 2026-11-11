// The debug-only `test-host` feature (loopback fixture URL, recorder instead of ShellExecute)
// must never be compilable into a release-profile binary, whichever way the profile is tuned.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const manifest = fileURLToPath(new URL('../rust/Cargo.toml', import.meta.url));
// Do not inherit caller profile overrides: each case states its own environment explicitly.
const baseEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('CARGO_PROFILE_')));

function cargoCheck(args, env = {}) {
  const result = spawnSync('cargo', ['+1.99.1', 'check', '--manifest-path', manifest, '--locked', ...args], {
    encoding: 'utf8',
    env: { ...baseEnv, ...env },
    timeout: 15 * 60 * 1000,
  });
  if (result.error) throw result.error;
  return { status: result.status, output: `${result.stdout}\n${result.stderr}` };
}

test('debug build with test-host compiles (positive control)', () => {
  const { status, output } = cargoCheck(['--features', 'test-host', '--lib', '--bins']);
  assert.equal(status, 0, output);
});

test('release + test-host is rejected', () => {
  const { status, output } = cargoCheck(['--features', 'test-host', '--release']);
  assert.notEqual(status, 0);
  assert.match(output, /NIMBO_TEST_HOST_FORBIDDEN_IN_RELEASE/);
});

test('release profile with debug assertions forced on is still rejected', () => {
  const { status, output } = cargoCheck(['--features', 'test-host', '--release'], {
    CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS: 'true',
  });
  assert.notEqual(status, 0);
  assert.match(output, /NIMBO_TEST_HOST_FORBIDDEN_IN_RELEASE/);
});

test('dev profile with debug assertions disabled is rejected at compile time', () => {
  const { status, output } = cargoCheck(['--features', 'test-host', '--lib'], {
    CARGO_PROFILE_DEV_DEBUG_ASSERTIONS: 'false',
  });
  assert.notEqual(status, 0);
  assert.match(output, /NIMBO_TEST_HOST_REQUIRES_DEBUG_ASSERTIONS/);
});

test('release without test-host still compiles', () => {
  const { status, output } = cargoCheck(['--release', '--lib', '--bins']);
  assert.equal(status, 0, output);
});
