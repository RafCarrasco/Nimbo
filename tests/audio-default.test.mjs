// Testes do som padrão da live (src/live/audioDefault.ts). Rode com `npm run test:live`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { defaultAudio, migrateAudio } from '../src/live/audioDefault.ts';

test('no app o padrão é só o som do jogo; no navegador, o do computador', () => {
  assert.equal(defaultAudio(true), 'window');
  assert.equal(defaultAudio(false), 'system');
});

test('quem estava no padrão antigo passa para o novo uma vez, só no app', () => {
  assert.equal(migrateAudio('system', true, false), 'window');
  // Depois da troca, escolher "Computador" de novo vale.
  assert.equal(migrateAudio('system', true, true), 'system');
  // No navegador nada muda.
  assert.equal(migrateAudio('system', false, false), 'system');
  // As outras escolhas ficam como estão.
  assert.equal(migrateAudio('none', true, false), 'none');
  assert.equal(migrateAudio('window', true, false), 'window');
});
