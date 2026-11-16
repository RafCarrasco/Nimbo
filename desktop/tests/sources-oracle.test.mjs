// The actual unchanged Electron function with existing VM/native doubles.
// No real Windows enumeration, thumbnails, capture or picker rendering is proved here.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createOracle } from './helpers/oracle.mjs';

const fixture = JSON.parse(readFileSync(new URL('../rust/tests/fixtures/sources-electron.json', import.meta.url), 'utf8'));
const sha256 = (relative) => createHash('sha256').update(readFileSync(new URL(relative, import.meta.url))).digest('hex');
const plain = value => JSON.parse(JSON.stringify(value));

test('frozen source vectors identify the actual Electron source and native-double helper', () => {
  assert.equal(sha256('../main.cjs'), fixture.provenance.electronSha256);
  assert.equal(sha256('./helpers/oracle.mjs'), fixture.provenance.oracleSha256);
  assert.equal(new Set(fixture.cases.map(c => c.name)).size, fixture.cases.length);
});

for (const c of fixture.cases) {
  test(`unchanged Electron source vector: ${c.name}`, async () => {
    const h = await createOracle({ nativeWindows: c.windows, nativeProcesses: c.processes });
    try {
      // Explicitly configure existing doubles only; never replace missingSources itself.
      h.evaluate('app.getAppMetrics = () => ' + JSON.stringify(c.own.map(pid => ({ pid }))));
      h.evaluate('app.getFileIcon = async p => { if(' + JSON.stringify(c.iconFailures || []) + '.includes(p)) throw new Error("fixture icon unavailable"); return {toDataURL:()=>"data:native-icon"} }');
      assert.deepEqual(plain(h.evaluate('GAMES')), fixture.games);
      assert.deepEqual(plain(await h.evaluate('missingSources(' + JSON.stringify(c.known.map(id => ({ id }))) + ')')), c.expected);
      assert.equal(h.servers.length, 0, 'source vectors never invoke the real loopback server');
      assert.equal(h.children.length, 0, 'source vectors never start even a fake audio/capture child');
    } finally { await h.close(); }
  });
}
