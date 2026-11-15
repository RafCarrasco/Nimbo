// Teste de rede descartável, segunda parte: este servidor se comporta como uma rede "difícil" (a
// porta pública muda a cada destino, como em muitos provedores com CGNAT) e oferece duas conexões.
// Do outro lado, na máquina de quem desenvolve, uma é atendida por um programa com regra de entrada
// no Firewall do Windows e a outra por um programa sem regra. Só tipos e contagens vão para o registro.
import puppeteer from 'puppeteer-core';
import crypto from 'node:crypto';
import dgram from 'node:dgram';
import { promises as dns } from 'node:dns';
import fs from 'node:fs';

const key = Buffer.from(process.env.REMOTE_TEST_KEY || '', 'hex');
if (key.length !== 32) throw new Error('REMOTE_TEST_KEY ausente ou inválida');
const id = fs.readFileSync(new URL('./run.txt', import.meta.url), 'utf8').trim().split(/\s+/).pop();
const branch = process.env.GITHUB_REF_NAME;
const base = `https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/contents/.github/remote-viewer`;
const headers = {
  Authorization: `Bearer ${process.env.GITHUB_TOKEN}`,
  Accept: 'application/vnd.github+json',
  'X-GitHub-Api-Version': '2022-11-28',
};
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

async function natProbe() {
  const servers = [
    ['stun.l.google.com', 19302],
    ['stun1.l.google.com', 19302],
    ['stun.cloudflare.com', 3478],
  ];
  const socket = dgram.createSocket('udp4');
  await new Promise((r) => socket.bind(0, r));
  const pending = new Map();
  socket.on('message', (msg) => {
    const done = pending.get(msg.subarray(8, 20).toString('hex'));
    if (!done) return;
    let i = 20;
    while (i + 4 <= msg.length) {
      const kind = msg.readUInt16BE(i);
      const len = msg.readUInt16BE(i + 2);
      if (kind === 0x0020 && msg[i + 5] === 0x01) {
        const ip = [0, 1, 2, 3].map((k) => msg[i + 8 + k] ^ [0x21, 0x12, 0xa4, 0x42][k]).join('.');
        return done({ ip, port: msg.readUInt16BE(i + 6) ^ 0x2112 });
      }
      i += 4 + Math.ceil(len / 4) * 4;
    }
  });
  const seen = [];
  for (const [host, port] of servers) {
    const address = await dns.lookup(host, { family: 4 }).then((r) => r.address, () => null);
    if (!address) continue;
    const tid = crypto.randomBytes(12);
    const answer = await new Promise((resolve) => {
      pending.set(tid.toString('hex'), resolve);
      socket.send(Buffer.concat([Buffer.from([0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]), tid]), port, address);
      setTimeout(() => resolve(null), 1500);
    });
    if (answer) seen.push(answer);
  }
  socket.close();
  return {
    respostas: seen.length,
    mesmoEndereco: new Set(seen.map((s) => s.ip)).size === 1,
    portasIguais: new Set(seen.map((s) => s.port)).size === 1,
  };
}

const browser = await puppeteer.launch({
  executablePath: process.env.CHROME_PATH || '/usr/bin/google-chrome',
  headless: 'new',
  args: ['--no-sandbox', '--disable-features=WebRtcHideLocalIpsWithMdns'],
});
try {
  console.log('NAT do servidor:', JSON.stringify(await natProbe()));
  const page = await browser.newPage();
  await page.goto('about:blank');
  const offers = await page.evaluate(async () => {
    window.kind = (addr) =>
      addr.endsWith('.local')
        ? 'nome-mdns'
        : addr.includes(':')
          ? 'ipv6'
          : /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(addr)
            ? 'ipv4-privado'
            : /^26\./.test(addr)
              ? 'ipv4-radmin'
              : 'ipv4-publico';
    window.pcs = {};
    const make = async (name) => {
      const pc = new RTCPeerConnection({
        iceServers: [
          { urls: ['stun:stun.l.google.com:19302', 'stun:stun1.l.google.com:19302'] },
          { urls: 'stun:stun.cloudflare.com:3478' },
        ],
      });
      const states = [];
      const t0 = performance.now();
      const note = (text) => states.push(`${Math.round(performance.now() - t0)}ms ${text}`);
      pc.oniceconnectionstatechange = () => note(`ice ${pc.iceConnectionState}`);
      pc.onconnectionstatechange = () => note(`conexao ${pc.connectionState}`);
      const channel = pc.createDataChannel('teste');
      channel.onopen = () => note('canal aberto');
      await pc.setLocalDescription();
      await new Promise((r) => {
        pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r();
        setTimeout(r, 3000);
      });
      window.pcs[name] = { pc, states };
      return pc.localDescription.toJSON();
    };
    return { a: await make('a'), b: await make('b') };
  });
  const put = await fetch(`${base}/offers-${id}.enc`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({
      message: `test: offers ${id}`,
      branch,
      content: Buffer.from(seal(JSON.stringify(offers))).toString('base64'),
    }),
  });
  if (!put.ok) throw new Error(`não deu para publicar as ofertas: ${put.status}`);
  console.log('ofertas publicadas; esperando as respostas');

  let answers = null;
  for (let i = 0; i < 240 && !answers; i++) {
    await sleep(1000);
    const res = await fetch(`${base}/answers-${id}.enc?ref=${branch}`, { headers, cache: 'no-store' });
    if (res.ok) answers = JSON.parse(open(Buffer.from((await res.json()).content, 'base64').toString('utf8')));
  }
  if (!answers) throw new Error('as respostas não chegaram em 4 minutos');

  const result = await page.evaluate(async (remote) => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const names = Object.keys(window.pcs).filter((name) => remote[name]);
    for (const name of names) await window.pcs[name].pc.setRemoteDescription(remote[name]);
    const settled = () => names.every((name) => ['connected', 'failed'].includes(window.pcs[name].pc.connectionState));
    for (let i = 0; i < 60 && !settled(); i++) await wait(500);
    const out = {};
    for (const name of names) {
      const { pc, states } = window.pcs[name];
      const stats = await pc.getStats();
      const byId = new Map();
      stats.forEach((s) => byId.set(s.id, s));
      const pairs = [];
      stats.forEach((s) => {
        if (s.type !== 'candidate-pair') return;
        const local = byId.get(s.localCandidateId) || {};
        const other = byId.get(s.remoteCandidateId) || {};
        pairs.push(
          `${local.candidateType}:${window.kind(local.address || '')} -> ${other.candidateType}:${window.kind(other.address || '')} ` +
            `${s.state}${s.nominated ? ' (escolhido)' : ''} pedidos ${s.requestsSent}/${s.responsesReceived} recebidos ${s.requestsReceived}/${s.responsesSent}`,
        );
      });
      out[name] = { estado: pc.connectionState, ice: pc.iceConnectionState, pares: pairs, estados: states };
    }
    return out;
  }, answers);
  console.log('RESULTADO', JSON.stringify(result, null, 1));
} finally {
  await browser.close();
}
