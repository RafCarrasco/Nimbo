// Testes da ordem IPv4 → IPv6 ao assistir uma live do app (src/live/iceFamily.ts). Rode com `npm run test:live`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { V6_AFTER_MS, v4First } from '../src/live/iceFamily.ts';

// Resposta no formato que o motor devolve: um transporte só, endereços na primeira seção.
const answer = (candidates, eol = '\r\n') =>
  [
    'v=0',
    'o=- 1 2 IN IP4 0.0.0.0',
    's=-',
    't=0 0',
    'a=group:BUNDLE 0 1',
    'm=video 9 UDP/TLS/RTP/SAVPF 102',
    'c=IN IP4 0.0.0.0',
    ...candidates,
    'a=ice-ufrag:abcd',
    'a=ice-pwd:0123456789abcdef012345',
    'a=mid:0',
    'a=sendonly',
    'm=audio 9 UDP/TLS/RTP/SAVPF 111',
    'c=IN IP4 0.0.0.0',
    'a=mid:1',
    'a=sendonly',
    '',
  ].join(eol);

const HOST4 = 'a=candidate:1 1 udp 2130706431 192.168.15.7 50000 typ host';
const SRFLX4 = 'a=candidate:2 1 udp 1694498815 203.0.113.9 50001 typ srflx raddr 192.168.15.7 rport 50000';
const HOST6A = 'a=candidate:3 1 udp 2130706431 2001:db8::10 50002 typ host';
const HOST6B = 'a=candidate:4 1 udp 2130706431 2001:db8:1::20 50003 typ host';

test('os endereços IPv6 do transmissor saem da resposta e ficam para depois', () => {
  const { sdp, later } = v4First(answer([HOST4, SRFLX4, HOST6A, HOST6B]));
  assert.ok(sdp.includes(HOST4) && sdp.includes(SRFLX4));
  assert.ok(!sdp.includes('2001:db8'));
  assert.deepEqual(later, [
    { candidate: HOST6A.slice(2), sdpMLineIndex: 0, sdpMid: '0' },
    { candidate: HOST6B.slice(2), sdpMLineIndex: 0, sdpMid: '0' },
  ]);
  // O resto da resposta não muda: mesma quantidade de linhas menos as duas tiradas, mesmo fim de linha.
  assert.equal(sdp.split('\r\n').length, answer([HOST4, SRFLX4, HOST6A, HOST6B]).split('\r\n').length - 2);
  assert.ok(sdp.endsWith('a=sendonly\r\n'));
});

test('o endereço de cada seção leva o mid e o índice dela, mesmo com o a=mid depois do endereço', () => {
  const sdp = ['v=0', 'm=video 9 X 1', HOST4, 'a=mid:video', 'm=audio 9 X 2', HOST6A, 'a=mid:som', ''].join('\n');
  const { sdp: first, later } = v4First(sdp);
  assert.deepEqual(later, [{ candidate: HOST6A.slice(2), sdpMLineIndex: 1, sdpMid: 'som' }]);
  assert.equal(first, ['v=0', 'm=video 9 X 1', HOST4, 'a=mid:video', 'm=audio 9 X 2', 'a=mid:som', ''].join('\n'));
});

test('sem IPv4 na resposta, ou sem IPv6, nada muda', () => {
  const only6 = answer([HOST6A, HOST6B]);
  assert.deepEqual(v4First(only6), { sdp: only6, later: [] });
  const only4 = answer([HOST4, SRFLX4]);
  assert.deepEqual(v4First(only4), { sdp: only4, later: [] });
  // Endereço IPv4 só por TCP não é caminho que o motor use: não segura o IPv6.
  const tcp4 = answer(['a=candidate:9 1 tcp 1518280447 192.168.15.7 9 typ host tcptype active', HOST6A]);
  assert.deepEqual(v4First(tcp4), { sdp: tcp4, later: [] });
  assert.deepEqual(v4First(''), { sdp: '', later: [] });
});

test('a espera cobre o primeiro teste do IPv4 e fica bem abaixo do prazo da tentativa', () => {
  // Medido: o primeiro teste do endereço público IPv4 sai em ~0,2 s; a tentativa inteira tem 12 s.
  assert.ok(V6_AFTER_MS >= 500 && V6_AFTER_MS <= 2000);
});
