// Teste de rede descartável, terceira parte: papéis como na live nativa de verdade. A máquina de quem
// desenvolve faz a oferta (como quem assiste) e este servidor responde e começa a testar os caminhos
// na hora (como o motor de quem transmite), antes de a resposta chegar do outro lado.
import puppeteer from 'puppeteer-core';
import crypto from 'node:crypto';
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

const res = await fetch(`${base}/offers-${id}.enc?ref=${branch}`, { headers, cache: 'no-store' });
if (!res.ok) throw new Error(`ofertas não encontradas: ${res.status}`);
const offers = JSON.parse(open(Buffer.from((await res.json()).content, 'base64').toString('utf8')));

const browser = await puppeteer.launch({
  executablePath: process.env.CHROME_PATH || '/usr/bin/google-chrome',
  headless: 'new',
  args: ['--no-sandbox', '--disable-features=WebRtcHideLocalIpsWithMdns'],
});
try {
  const page = await browser.newPage();
  await page.goto('about:blank');
  const answers = await page.evaluate(async (remote) => {
    window.kind = (addr) =>
      !addr ? 'oculto' : addr.endsWith('.local') ? 'nome-mdns' : addr.includes(':') ? 'ipv6' : /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(addr) ? 'ipv4-privado' : /^26\./.test(addr) ? 'ipv4-radmin' : 'ipv4-publico';
    window.pcs = {};
    const out = {};
    for (const name of Object.keys(remote)) {
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
      pc.ondatachannel = (e) => {
        e.channel.onopen = () => note('canal aberto');
      };
      await pc.setRemoteDescription(remote[name]);
      // A partir daqui este lado já manda testes de caminho, como o motor faz ao aceitar a oferta.
      await pc.setLocalDescription(await pc.createAnswer());
      await new Promise((r) => {
        pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r();
        setTimeout(r, 3000);
      });
      window.pcs[name] = { pc, states };
      out[name] = pc.localDescription.toJSON();
    }
    return out;
  }, offers);
  const put = await fetch(`${base}/answers-${id}.enc`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ message: `test: answers ${id}`, branch, content: Buffer.from(seal(JSON.stringify(answers))).toString('base64') }),
  });
  if (!put.ok) throw new Error(`não deu para publicar as respostas: ${put.status}`);
  console.log('respostas publicadas; este lado já está mandando testes de caminho');
  const result = await page.evaluate(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const names = Object.keys(window.pcs);
    const settled = () => names.every((name) => ['connected', 'failed'].includes(window.pcs[name].pc.connectionState));
    for (let i = 0; i < 70 && !settled(); i++) await wait(500);
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
          `${local.candidateType}:${window.kind(local.address)} -> ${other.candidateType}:${window.kind(other.address)} ${s.state}${s.nominated ? ' (escolhido)' : ''} pedidos ${s.requestsSent}/${s.responsesReceived} recebidos ${s.requestsReceived}/${s.responsesSent}`,
        );
      });
      out[name] = { estado: pc.connectionState, pares: pairs, estados: states };
    }
    return out;
  });
  console.log('RESULTADO', JSON.stringify(result, null, 1));
} finally {
  await browser.close();
}
