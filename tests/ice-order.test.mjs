// Testes da ordem do primeiro pacote (src/live/iceOrder.ts). Rode com `npm run test:live`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { iceUfrag, leaderOf, lesson, mustLead, orderFor, remember, withoutCandidates } from '../src/live/iceOrder.ts';

test('a primeira tentativa não interfere, a não ser que a rede precise mandar primeiro', () => {
  assert.equal(orderFor(0, true, false), 'natural');
  assert.equal(orderFor(0, false, false), 'natural');
  assert.equal(orderFor(0, true, true), 'me');
  assert.equal(orderFor(0, false, true), 'me');
});

test('depois de uma falha, os dois lados concordam em quem manda primeiro, e as duas ordens são tentadas', () => {
  const leaders = [];
  for (const attempt of [1, 2, 4, 5]) {
    const lower = orderFor(attempt, true, false);
    const higher = orderFor(attempt, false, false);
    // Cada lado age sozinho: um segura os endereços ('me'), o outro espera ('them'). Nunca os dois iguais.
    assert.notEqual(lower, 'natural');
    assert.notEqual(lower, higher);
    leaders.push(lower === 'me' ? 'menor' : 'maior');
  }
  assert.deepEqual(leaders, ['menor', 'maior', 'menor', 'maior']);
  // A cada três tentativas volta para a ordem de sempre.
  assert.equal(orderFor(3, true, false), 'natural');
});

test('na ordem natural quem respondeu à oferta manda primeiro', () => {
  assert.equal(leaderOf('natural', true), 'me');
  assert.equal(leaderOf('natural', false), 'them');
  assert.equal(leaderOf('me', false), 'me');
  assert.equal(leaderOf('them', true), 'them');
});

test('só marca a rede quando conectou mandando primeiro depois de falhar do outro jeito', () => {
  assert.equal(lesson('me', new Set(['them'])), 'lead');
  assert.equal(lesson('them', new Set(['me'])), 'free');
  assert.equal(lesson('me', new Set()), null);
  assert.equal(lesson('me', new Set(['me'])), null);
  assert.equal(lesson('them', new Set(['them'])), null);
});

test('a anotação da rede fica guardada por 30 dias e pode ser desfeita', () => {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, String(value)),
    removeItem: (key) => store.delete(key),
  };
  const now = 1_000_000;
  assert.equal(mustLead(now), false);
  remember(null, now);
  assert.equal(mustLead(now), false);
  remember('lead', now);
  assert.equal(mustLead(now), true);
  assert.equal(mustLead(now + 29 * 24 * 3600 * 1000), true);
  assert.equal(mustLead(now + 31 * 24 * 3600 * 1000), false);
  remember('free', now);
  assert.equal(mustLead(now), false);
  delete globalThis.localStorage;
  // Sem armazenamento (modo privado bloqueado): não quebra, só não lembra.
  assert.equal(mustLead(now), false);
  remember('lead', now);
});

test('a oferta sai sem endereços e a rodada é identificada', () => {
  const sdp = [
    'v=0',
    'a=ice-ufrag:AbCd',
    'a=candidate:1 1 udp 2122260223 192.168.15.7 61338 typ host',
    'a=candidate:2 1 udp 1686052607 200.1.2.3 61338 typ srflx raddr 192.168.15.7 rport 61338',
    'a=end-of-candidates',
    'a=rtcp-mux',
    '',
  ].join('\r\n');
  const out = withoutCandidates({ type: 'offer', sdp });
  assert.equal(out.type, 'offer');
  assert.equal(out.sdp.includes('a=candidate:'), false);
  assert.equal(out.sdp.includes('end-of-candidates'), false);
  assert.equal(out.sdp, ['v=0', 'a=ice-ufrag:AbCd', 'a=rtcp-mux', ''].join('\r\n'));
  assert.equal(iceUfrag(sdp), 'AbCd');
  assert.equal(iceUfrag(''), '');
  assert.equal(iceUfrag(undefined), '');
});
