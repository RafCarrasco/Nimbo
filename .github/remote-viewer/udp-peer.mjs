// Teste de rede descartável, quarta parte: só UDP, sem navegador. Mede o que o roteador do outro
// lado faz quando este servidor manda primeiro, em vários cenários, cada um num par de portas próprio.
import crypto from 'node:crypto';
import dgram from 'node:dgram';
import { promises as dns } from 'node:dns';
import fs from 'node:fs';

const key = Buffer.from(process.env.REMOTE_TEST_KEY || '', 'hex');
if (key.length !== 32) throw new Error('REMOTE_TEST_KEY ausente ou inválida');
const id = fs.readFileSync(new URL('./run.txt', import.meta.url), 'utf8').trim().split(/\s+/).pop();
const branch = process.env.GITHUB_REF_NAME;
const base = `https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/contents/.github/remote-viewer`;
const headers = { Authorization: `Bearer ${process.env.GITHUB_TOKEN}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const seal = (text) => {
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv('aes-256-gcm', key, iv);
  const body = Buffer.concat([cipher.update(text, 'utf8'), cipher.final()]);
  return Buffer.concat([iv, cipher.getAuthTag(), body]).toString('base64');
};
const open = (sealed) => {
  const raw = Buffer.from(sealed, 'base64');
  const decipher = crypto.createDecipheriv('aes-256-gcm', key, raw.subarray(0, 12));
  decipher.setAuthTag(raw.subarray(12, 28));
  return Buffer.concat([decipher.update(raw.subarray(28)), decipher.final()]).toString('utf8');
};
const stunServer = await dns.lookup('stun.cloudflare.com', { family: 4 }).then((r) => r.address);
// Endereço público de um soquete, perguntando a um servidor STUN.
const mapped = (socket) =>
  new Promise((resolve) => {
    const tid = crypto.randomBytes(12);
    const onMessage = (msg) => {
      if (msg.length < 20 || !msg.subarray(8, 20).equals(tid)) return;
      let i = 20;
      while (i + 4 <= msg.length) {
        const kind = msg.readUInt16BE(i);
        const len = msg.readUInt16BE(i + 2);
        if (kind === 0x0020 && msg[i + 5] === 0x01) {
          socket.off('message', onMessage);
          const ip = [0, 1, 2, 3].map((k) => msg[i + 8 + k] ^ [0x21, 0x12, 0xa4, 0x42][k]).join('.');
          return resolve({ ip, port: msg.readUInt16BE(i + 6) ^ 0x2112 });
        }
        i += 4 + Math.ceil(len / 4) * 4;
      }
    };
    socket.on('message', onMessage);
    socket.send(Buffer.concat([Buffer.from([0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]), tid]), 3478, stunServer);
    setTimeout(() => resolve(null), 2000);
  });

const COUNT = Number(process.env.SCENARIOS || 6);
const sockets = [];
for (let i = 0; i < COUNT; i++) {
  const socket = dgram.createSocket('udp4');
  await new Promise((r) => socket.bind(0, r));
  sockets.push({ socket, address: await mapped(socket), got: [] });
}
if (sockets.some((s) => !s.address)) throw new Error('STUN não respondeu neste servidor');
await fetch(`${base}/udpr-${id}.enc`, {
  method: 'PUT',
  headers,
  body: JSON.stringify({ message: `test: udp ${id}`, branch, content: Buffer.from(seal(JSON.stringify(sockets.map((s) => s.address)))).toString('base64') }),
});
let plan = null;
for (let i = 0; i < 240 && !plan; i++) {
  await sleep(1000);
  const res = await fetch(`${base}/udpl-${id}.enc?ref=${branch}`, { headers, cache: 'no-store' });
  if (res.ok) plan = JSON.parse(open(Buffer.from((await res.json()).content, 'base64').toString('utf8')));
}
if (!plan) throw new Error('o plano do outro lado não chegou');
// plan: { starts: [epoch ms por cenário], peers: [{ip, port}], sendMs }
await Promise.all(
  sockets.map(async (entry, i) => {
    const start = plan.starts[i];
    entry.socket.on('message', (msg) => {
      if (msg.toString().startsWith('nimbo-l')) entry.got.push(Date.now() - start);
    });
    await sleep(Math.max(0, start - Date.now()));
    for (let n = 0; Date.now() - start < plan.sendMs; n++) {
      entry.socket.send(Buffer.from(`nimbo-r${n}`), plan.peers[i].port, plan.peers[i].ip);
      await sleep(100);
    }
  }),
);
await sleep(1000);
console.log(
  'RESULTADO',
  JSON.stringify(
    sockets.map((s, i) => ({ cenario: i, recebidos: s.got.length, primeiroMs: s.got.length ? s.got[0] : null })),
    null,
    1,
  ),
);
for (const s of sockets) s.socket.close();
