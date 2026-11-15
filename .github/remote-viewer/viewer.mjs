// Teste de rede descartável: um Chrome num servidor do GitHub Actions assiste a uma live do motor
// rodando na máquina de quem desenvolve. Serve para ver se o motor conecta pela internet de verdade
// (outra rede, outro NAT). A oferta e a resposta trafegam cifradas por arquivos nesta branch; o
// registro do teste só mostra tipos de endereço e contagens, nunca os endereços.
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

// Que tipo de NAT este servidor tem: a porta pública muda conforme o destino?
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
  args: ['--no-sandbox', '--autoplay-policy=no-user-gesture-required', '--disable-features=WebRtcHideLocalIpsWithMdns'],
});
try {
  console.log('NAT do servidor:', JSON.stringify(await natProbe()));
  const page = await browser.newPage();
  await page.goto('about:blank');
  const offer = await page.evaluate(async () => {
    const kind = (addr) =>
      addr.endsWith('.local')
        ? 'nome-mdns'
        : addr.includes(':')
          ? 'ipv6'
          : /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(addr)
            ? 'ipv4-privado'
            : /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(addr)
              ? 'ipv4-cgnat'
              : /^26\./.test(addr)
                ? 'ipv4-radmin'
                : 'ipv4-publico';
    window.summary = (sdp) => {
      const out = {};
      for (const line of sdp.split(/\r?\n/)) {
        if (!line.startsWith('a=candidate')) continue;
        const p = line.split(' ');
        const name = `${p[7]}/${p[2].toLowerCase()}:${kind(p[4])}`;
        out[name] = (out[name] || 0) + 1;
      }
      return out;
    };
    window.kind = kind;
    const pc = new RTCPeerConnection({
      iceServers: [
        { urls: ['stun:stun.l.google.com:19302', 'stun:stun1.l.google.com:19302'] },
        { urls: 'stun:stun.cloudflare.com:3478' },
      ],
    });
    window.pc = pc;
    window.states = [];
    const t0 = performance.now();
    const note = () => window.states.push(`${Math.round(performance.now() - t0)}ms ${pc.iceConnectionState}/${pc.connectionState}`);
    pc.oniceconnectionstatechange = note;
    pc.onconnectionstatechange = note;
    pc.addTransceiver('video', { direction: 'recvonly' });
    pc.addTransceiver('audio', { direction: 'recvonly' });
    await pc.setLocalDescription();
    await new Promise((r) => {
      pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r();
      setTimeout(r, 3000);
    });
    return { sdp: pc.localDescription.toJSON(), candidatos: window.summary(pc.localDescription.sdp) };
  });
  console.log('oferta do espectador remoto:', JSON.stringify(offer.candidatos));

  const put = await fetch(`${base}/offer-${id}.enc`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({
      message: `test: offer ${id}`,
      branch,
      content: Buffer.from(seal(JSON.stringify(offer.sdp))).toString('base64'),
    }),
  });
  if (!put.ok) throw new Error(`não deu para publicar a oferta: ${put.status}`);
  console.log('oferta publicada; esperando a resposta do motor');

  let answer = null;
  for (let i = 0; i < 240 && !answer; i++) {
    await sleep(1000);
    const res = await fetch(`${base}/answer-${id}.enc?ref=${branch}`, { headers, cache: 'no-store' });
    if (res.ok) answer = JSON.parse(open(Buffer.from((await res.json()).content, 'base64').toString('utf8')));
  }
  if (!answer) throw new Error('a resposta do motor não chegou em 4 minutos');

  const result = await page.evaluate(async (remote) => {
    const pc = window.pc;
    const from = performance.now();
    await pc.setRemoteDescription(remote);
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    for (let i = 0; i < 40 && pc.connectionState !== 'connected' && pc.connectionState !== 'failed'; i++) await wait(500);
    const settled = Math.round(performance.now() - from);
    // Fica assistindo por um bom tempo: houve queda sempre aos 18 s, e 5 s de teste não mostravam.
    const watched = [];
    if (pc.connectionState === 'connected') {
      // A cada 5 s: quadros por segundo, banda, travadas, espera no buffer, pedidos de reenvio e de
      // quadro completo, perda e tempo de ida e volta.
      let before = null;
      for (let second = 1; second <= 45; second++) {
        await wait(1000);
        if (second % 5 !== 0) continue;
        const now = { frames: 0, bytes: 0, freezes: 0, frozen: 0, buffer: 0, emitted: 0, nack: 0, pli: 0, lost: 0, received: 0, size: '', rtt: 0 };
        (await pc.getStats()).forEach((s) => {
          if (s.type === 'inbound-rtp' && s.kind === 'video') {
            now.frames = s.framesDecoded || 0;
            now.bytes = s.bytesReceived || 0;
            now.freezes = s.freezeCount || 0;
            now.frozen = s.totalFreezesDuration || 0;
            now.buffer = s.jitterBufferDelay || 0;
            now.emitted = s.jitterBufferEmittedCount || 0;
            now.nack = s.nackCount || 0;
            now.pli = s.pliCount || 0;
            now.lost = s.packetsLost || 0;
            now.received = s.packetsReceived || 0;
            now.size = `${s.frameWidth}x${s.frameHeight}`;
          }
          if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') now.rtt = Math.round((s.currentRoundTripTime || 0) * 1000);
        });
        const d = before || { frames: 0, bytes: 0, freezes: 0, frozen: 0, buffer: 0, emitted: 0, nack: 0, pli: 0, lost: 0, received: 0 };
        const packets = now.received - d.received + (now.lost - d.lost);
        watched.push(
          `${second}s: ${((now.frames - d.frames) / 5).toFixed(0)} q/s, ${(((now.bytes - d.bytes) * 8) / 5e6).toFixed(1)} Mbps, ${now.size}, ` +
            `travadas ${now.freezes - d.freezes} (${(now.frozen - d.frozen).toFixed(1)} s), buffer ${now.emitted > d.emitted ? Math.round(((now.buffer - d.buffer) / (now.emitted - d.emitted)) * 1000) : 0} ms, ` +
            `reenvios ${now.nack - d.nack}, quadro completo ${now.pli - d.pli}, perda ${packets > 0 ? (((now.lost - d.lost) / packets) * 100).toFixed(1) : '0'}%, ida e volta ${now.rtt} ms, ${pc.connectionState}`,
        );
        before = now;
      }
    }
    window.watched = watched;
    const stats = await pc.getStats();
    const byId = new Map();
    stats.forEach((s) => byId.set(s.id, s));
    const pairs = [];
    let frames = 0;
    let bytes = 0;
    stats.forEach((s) => {
      if (s.type === 'inbound-rtp' && s.kind === 'video') {
        frames = s.framesDecoded || 0;
        bytes = s.bytesReceived || 0;
      }
      if (s.type !== 'candidate-pair') return;
      const local = byId.get(s.localCandidateId) || {};
      const remoteCandidate = byId.get(s.remoteCandidateId) || {};
      pairs.push(
        `${local.candidateType}/${local.protocol}:${window.kind(local.address || '')} -> ${remoteCandidate.candidateType}:${window.kind(remoteCandidate.address || '')} ` +
          `${s.state}${s.nominated ? ' (escolhido)' : ''} pedidos ${s.requestsSent}/${s.responsesReceived} recebidos ${s.requestsReceived}/${s.responsesSent}`,
      );
    });
    return {
      respostaDoMotor: window.summary(remote.sdp),
      estado: pc.connectionState,
      ice: pc.iceConnectionState,
      decididoEmMs: settled,
      quadros: frames,
      bytesDeVideo: bytes,
      pares: pairs,
      estados: window.states,
      assistido: window.watched,
    };
  }, answer);
  console.log('RESULTADO', JSON.stringify(result, null, 1));
} finally {
  await browser.close();
}
