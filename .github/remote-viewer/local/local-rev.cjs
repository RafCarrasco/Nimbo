// Lado local da terceira parte: os programas daqui fazem a oferta (como quem assiste uma live
// nativa) e o servidor remoto responde e já sai testando os caminhos (como o motor de quem transmite).
// Uso: node local-rev.cjs [upload] [atrasoMs]
const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const path = require('path');
const { spawn } = require('child_process');

const TOKEN = process.env.GH_TOKEN;
if (!TOKEN) throw new Error('GH_TOKEN ausente');
const key = Buffer.from(fs.readFileSync(path.join(__dirname, '..', 'remote-test.key'), 'utf8').trim(), 'hex');
const BRANCH = 'test/remote-viewer';
const base = 'https://api.github.com/repos/RafCarrasco/Nimbo/contents';
const headers = { Authorization: `Bearer ${TOKEN}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
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
const put = async (file, text, message) => {
  const current = await fetch(`${base}/${file}?ref=${BRANCH}`, { headers });
  const sha = current.ok ? (await current.json()).sha : undefined;
  const res = await fetch(`${base}/${file}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ message, branch: BRANCH, content: Buffer.from(text).toString('base64'), sha }),
  });
  if (!res.ok) throw new Error(`PUT ${file}: ${res.status} ${(await res.text()).slice(0, 200)}`);
};
const delay = Number(process.argv.find((a) => /^\d+$/.test(a)) || 0);

const PAGE = `<!doctype html><meta charset="utf-8"><title>Teste de rede do Nimbo (fecha sozinho)</title>
<body style="background:#0b1020;color:#cbd5e1;font:16px system-ui;padding:24px">Teste de rede do Nimbo em andamento. Esta janela fecha sozinha.
<script>
const who = new URLSearchParams(location.search).get('who');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const kind = (addr) => !addr ? 'oculto' : addr.endsWith('.local') ? 'nome-mdns' : addr.includes(':') ? 'ipv6' : /^(10\\.|192\\.168\\.|172\\.(1[6-9]|2\\d|3[01])\\.)/.test(addr) ? 'ipv4-privado' : /^26\\./.test(addr) ? 'ipv4-radmin' : 'ipv4-publico';
const call = async (url, body) => { for (;;) { try { const r = await fetch(url, body ? { method: 'POST', body: JSON.stringify(body) } : undefined); if (r.ok) return r.json(); } catch {} await sleep(300); } };
(async () => {
  const pc = new RTCPeerConnection({ iceServers: [{ urls: ['stun:stun.l.google.com:19302', 'stun:stun1.l.google.com:19302'] }, { urls: 'stun:stun.cloudflare.com:3478' }] });
  const states = []; const t0 = performance.now();
  const note = (text) => states.push(Math.round(performance.now() - t0) + 'ms ' + text);
  pc.oniceconnectionstatechange = () => note('ice ' + pc.iceConnectionState);
  pc.onconnectionstatechange = () => note('conexao ' + pc.connectionState);
  const hold = new URLSearchParams(location.search).get('hold') === '1';
  const mine = [];
  pc.onicecandidate = (e) => { if (e.candidate && e.candidate.candidate) mine.push(e.candidate.toJSON()); };
  const channel = pc.createDataChannel('teste');
  channel.onopen = () => note('canal aberto');
  await pc.setLocalDescription();
  await new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); setTimeout(r, 2500); });
  const offer = pc.localDescription.toJSON();
  if (hold) offer.sdp = offer.sdp.split('\\n').filter((line) => !/^a=(candidate:|end-of-candidates)/.test(line)).join('\\n');
  await call('/offer?who=' + who, offer);
  const answer = await call('/answer?who=' + who);
  note('resposta chegou');
  await sleep(${delay});
  await pc.setRemoteDescription(answer);
  note('resposta aplicada');
  if (hold) { await sleep(1500); await call('/ice?who=' + who, mine); note('enderecos entregues: ' + mine.length); }
  for (let i = 0; i < 60 && !['connected', 'failed'].includes(pc.connectionState); i++) await sleep(500);
  const stats = await pc.getStats(); const byId = new Map(); stats.forEach((s) => byId.set(s.id, s));
  const pairs = [];
  stats.forEach((s) => { if (s.type !== 'candidate-pair') return; const l = byId.get(s.localCandidateId) || {}; const o = byId.get(s.remoteCandidateId) || {};
    pairs.push(l.candidateType + ':' + kind(l.address) + ' -> ' + o.candidateType + ':' + kind(o.address) + ' ' + s.state + (s.nominated ? ' (escolhido)' : '') + ' pedidos ' + s.requestsSent + '/' + s.responsesReceived + ' recebidos ' + s.requestsReceived + '/' + s.responsesSent); });
  await call('/result?who=' + who, { estado: pc.connectionState, pares: pairs, estados: states });
})();
</script>`;

(async () => {
  if (process.argv.includes('upload')) {
    await put('.github/remote-viewer/peer-rev.mjs', fs.readFileSync(path.join(__dirname, 'peer-rev.mjs'), 'utf8'), 'test: reversed roles');
    await put('.github/workflows/remote-viewer-test.yml', fs.readFileSync(path.join(__dirname, 'remote-viewer-test.yml'), 'utf8'), 'test: modes');
    console.log('arquivos do teste enviados');
  }
  const id = crypto.randomBytes(6).toString('hex');
  const t0 = Date.now();
  const at = () => `+${Math.round((Date.now() - t0) / 1000)}s`;
  const offers = {};
  const answers = {};
  const results = {};
  const ices = {};
  const hold = process.argv.includes('hold');
  const slot = { electron: 'a', webview: 'b' };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const who = url.searchParams.get('who');
    const send = (code, body, type = 'application/json') => {
      res.writeHead(code, { 'Content-Type': type, 'Cache-Control': 'no-store' });
      res.end(body);
    };
    if (req.method === 'GET' && url.pathname === '/') return send(200, PAGE, 'text/html; charset=utf-8');
    if (req.method === 'GET' && url.pathname === '/answer') return answers[slot[who]] ? send(200, JSON.stringify(answers[slot[who]])) : send(404, '{}');
    if (req.method === 'POST') {
      let body = '';
      req.on('data', (c) => (body += c));
      req.on('end', () => {
        if (url.pathname === '/offer') offers[slot[who]] = JSON.parse(body);
        if (url.pathname === '/result') results[who] = JSON.parse(body);
        if (url.pathname === '/ice') {
          ices[slot[who]] = JSON.parse(body);
          if (ices.a && ices.b) void put(`.github/remote-viewer/ice-${id}.enc`, seal(JSON.stringify(ices)), `test: ice ${id}`).then(() => console.log(at(), 'endereços locais publicados'));
        }
        send(200, '{}');
      });
      return;
    }
    send(404, '{}');
  });
  await new Promise((r) => server.listen(3071, '127.0.0.1', r));
  const electronMain = path.join(__dirname, 'electron-main.cjs');
  fs.writeFileSync(
    electronMain,
    `const { app, BrowserWindow } = require('electron');
app.setPath('userData', ${JSON.stringify(path.join(__dirname, 'ud-electron'))});
app.commandLine.appendSwitch('disable-renderer-backgrounding');
app.whenReady().then(() => {
  const w = new BrowserWindow({ width: 520, height: 200, show: false, webPreferences: { backgroundThrottling: false } });
  w.loadURL('http://127.0.0.1:3071/?who=electron${hold ? '&hold=1' : ''}');
});`,
  );
  const children = [
    spawn('D:\\Nimbo\\node_modules\\electron\\dist\\electron.exe', [electronMain], { stdio: 'ignore' }),
    spawn('D:\\Nimbo\\desktop\\rust-app\\target\\release\\nimbo-app.exe', [], {
      stdio: 'ignore',
      env: { ...process.env, NIMBO_URL: 'http://127.0.0.1:3071/?who=webview' + (hold ? '&hold=1' : ''), NIMBO_APP_ID: 'app.nimbo.testfw', NIMBO_DATA_DIR: path.join(__dirname, 'ud-webview') },
    }),
  ];
  try {
    for (let i = 0; i < 80 && !(offers.a && offers.b); i++) await sleep(250);
    if (!offers.a || !offers.b) throw new Error('as ofertas locais não saíram: ' + Object.keys(offers).join(','));
    await put(`.github/remote-viewer/offers-${id}.enc`, seal(JSON.stringify(offers)), `test: offers ${id}`);
    await put('.github/remote-viewer/run.txt', `${process.argv.includes('quiet') ? 'revq' : hold ? 'revh' : 'rev'} ${id}\n`, `test: run rev ${id}`);
    console.log(at(), 'ofertas locais publicadas e teste disparado', id, 'atraso extra', delay, 'ms');
    let remote = null;
    for (let i = 0; i < 200 && !remote; i++) {
      await sleep(1000);
      const res = await fetch(`${base}/.github/remote-viewer/answers-${id}.enc?ref=${BRANCH}`, { headers, cache: 'no-store' });
      if (res.ok) remote = JSON.parse(open(Buffer.from((await res.json()).content, 'base64').toString('utf8')));
    }
    if (!remote) throw new Error('as respostas do servidor remoto não chegaram');
    Object.assign(answers, remote);
    console.log(at(), 'respostas recebidas (o lado remoto já estava mandando)');
    for (let i = 0; i < 140 && !(results.electron && results.webview); i++) await sleep(500);
    console.log('LOCAL', JSON.stringify(results, null, 1));
  } finally {
    for (const child of children) child.kill();
    server.close();
  }
  process.exit(0);
})().catch((e) => {
  console.error('ERRO', e.message);
  process.exit(1);
});
