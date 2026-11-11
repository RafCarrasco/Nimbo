import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { APPROVED_EXCEPTIONS, WINDOWS_PREVIEW } from '../scripts/check-scope.mjs';

const workflow = readFileSync(new URL('../../.github/workflows/windows.yml', import.meta.url), 'utf8');

test('Windows PREVIEW is manual-only, read-only, pinned Rust and artifact-only', () => {
  // Check the entire blob, not only matching safe-looking YAML keys. Extra jobs,
  // aliases, duplicate keys and commands inside multiline text cannot sneak in.
  const bytes = Buffer.from(workflow);
  const oid = createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
  assert.equal(oid, 'bfcea98e83177ec568dfb79ca31723ce176bb060');
  assert.deepEqual(WINDOWS_PREVIEW, { path: '.github/workflows/windows.yml', oid });
  assert.ok(APPROVED_EXCEPTIONS.some((e) => e.path === WINDOWS_PREVIEW.path && e.oid === oid));
  assert.match(workflow, /^name: Nimbo Rust Windows PREVIEW not release$/m);
  assert.match(workflow, /^on:\n  workflow_dispatch:\npermissions:\n  contents: read\njobs:\n/m);
  assert.equal(workflow.match(/runs-on:/g).length, 1, 'single job');
  assert.match(workflow, /runs-on: windows-latest/);
  assert.match(workflow, /persist-credentials: false/);
  assert.match(workflow, /rustup toolchain install 1\.99\.1 --profile minimal --component rustfmt --component clippy/);
  assert.equal(readFileSync(new URL('../rust/rust-toolchain.toml', import.meta.url), 'utf8'), '[toolchain]\nchannel = "1.99.1"\nprofile = "minimal"\ncomponents = ["rustfmt", "clippy"]\n');
  assert.match(workflow, /cargo \+1\.99\.1 fmt --manifest-path desktop\/rust\/Cargo.toml --all -- --check/);
  for (const command of ['clippy', 'test', 'build']) {
    assert.ok(workflow.includes(`cargo +1.99.1 ${command} --manifest-path desktop/rust/Cargo.toml --locked`), command);
  }
  for (const line of workflow.split('\n').filter((l) => l.includes('cargo +1.99.1 ') && !l.includes(' fmt '))) {
    assert.match(line, /--locked/, line);
  }
  assert.match(workflow, /clippy[^\n]+--all-targets -- -D warnings/);
  assert.match(workflow, /clippy[^\n]+--all-targets --features test-host -- -D warnings/);
  assert.match(workflow, /test [^\n]+--locked --features test-host\n/);
  assert.match(workflow, /build[^\n]+--locked --release\n/);
  assert.doesNotMatch(workflow, /build[^\n]+test-host/, 'the artifact is never a test-host build');
  assert.doesNotMatch(workflow, /--ignored/, 'interactive tests are operator-run, not CI claims');
  assert.match(workflow, /desktop\/rust\/target\/release\/nimbo-desktop.exe/);
  assert.match(workflow, /Get-FileHash[^\n]+-Algorithm SHA256/);
  assert.match(workflow, /size_bytes = \(Get-Item/);
  assert.match(workflow, /commit = \$env:GITHUB_SHA/);
  assert.match(workflow, /name: Nimbo-Rust-Windows-PREVIEW-not-release\n/);
  assert.match(workflow, /if-no-files-found: error/);
  assert.match(workflow, /not an installer, not a release/);
  assert.match(workflow, /does not execute this executable/);
  assert.match(workflow, /No release, production upload, signing, deployment or update is performed/);
  assert.doesNotMatch(workflow, /electron|npm |desktop:pack|secrets|contents: write|gh release|git push|latest\.yml|Nimbo-Setup|updater/i);
});
