// EVIDENCE (detection level) for the S2-integration browser tiers. This is NOT the no-network
// guarantee: the browser tiers run only inside the approved network-none container, whose
// kernel namespace makes any external route impossible. These collectors only DETECT attempts:
// a CDP URL log (Network.* events, masked) and a /proc socket audit of every process at the end
// of a tier. Any non-loopback entry fails the tier even though the namespace made it unroutable.
// Nothing here opens a socket. Recorded URLs never carry the login state, tokens or keys.
import fs from 'node:fs';
import path from 'node:path';

const LOOPBACK_SCHEMES = new Set(['http:', 'https:', 'ws:', 'wss:']);
const INTERNAL_SCHEMES = new Set(['about:', 'data:', 'blob:', 'chrome-error:']);

/** IPv4 127.0.0.0/8 or IPv6 ::1 / ::ffff:127.x (literal hostnames only; no DNS). */
function loopbackHostname(hostname) {
  if (hostname === 'localhost' || hostname === '[::1]') return true;
  if (/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(hostname)) return hostname.split('.').every((n) => Number(n) <= 255);
  return /^\[::ffff:127\.\d{1,3}\.\d{1,3}\.\d{1,3}\]$/.test(hostname) || /^\[::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}\]$/.test(hostname);
}

/** 'loopback' | 'internal' | 'external'. Unknown or unparsable input is external (fail closed). */
export function classifyUrl(url) {
  let u;
  try {
    u = new URL(String(url));
  } catch {
    return 'external';
  }
  if (INTERNAL_SCHEMES.has(u.protocol)) return 'internal';
  if (LOOPBACK_SCHEMES.has(u.protocol) && loopbackHostname(u.hostname)) return 'loopback';
  return 'external';
}

const SECRET_PARAMS = new Set(['state', 'idtoken', 'accesstoken', 'id_token', 'access_token', 'refresh_token', 'refreshtoken',
  'key', 'apikey', 'token', 'code', 'oobcode', 'secret', 'password', 'credential', 'postbody']);

/** Host/path kept; secret-bearing query values, the whole fragment and long data: URLs masked. */
export function maskUrl(url) {
  const s = String(url);
  if (s.startsWith('data:')) {
    const c = s.indexOf(',');
    return `${s.slice(0, c > 0 ? Math.min(c, 48) : 48)},<${s.length} chars>`;
  }
  let u;
  try {
    u = new URL(s);
  } catch {
    return '<unparsable url>';
  }
  for (const k of [...u.searchParams.keys()]) {
    if (SECRET_PARAMS.has(k.toLowerCase())) u.searchParams.set(k, '<masked>');
  }
  if (u.hash) u.hash = '#<masked>';
  return u.href.replace(/%3Cmasked%3E/g, '<masked>');
}

// ---------------------------------------------------------------- run guard
/**
 * The browser tiers run ONLY inside the approved network-none container: the network namespace
 * must have the loopback interface alone and no non-loopback route, and the container's
 * allowlisted NIMBO_CHROME / NIMBO_BRIDGE_STDIO_BIN must be set. Anywhere else (for example the
 * coordinator host) the tiers are refused before any browser, server or emulator traffic.
 */
export function isolationGuard({ procRoot = '/proc', env = process.env, fs: f = fs } = {}) {
  const read = (p) => { try { return f.readFileSync(p, 'utf8'); } catch { return null; } };
  const dev = read(path.join(procRoot, 'net/dev'));
  const interfaces = dev === null ? null : dev.split('\n').slice(2).map((l) => l.split(':')[0].trim()).filter(Boolean);
  const route = read(path.join(procRoot, 'net/route'));
  const v4 = route === null ? null : route.split('\n').slice(1).map((l) => l.trim().split(/\s+/)[0]).filter(Boolean).filter((d) => d !== 'lo');
  const r6 = read(path.join(procRoot, 'net/ipv6_route'));
  const v6 = r6 === null ? [] : r6.split('\n').map((l) => l.trim().split(/\s+/).at(-1)).filter(Boolean).filter((d) => d !== 'lo');
  const reasons = [];
  if (!(interfaces && interfaces.length === 1 && interfaces[0] === 'lo')) reasons.push(`interfaces ${JSON.stringify(interfaces)} are not exactly [lo]`);
  if (v4 === null || v4.length || v6.length) reasons.push('a non-loopback route exists (or routes unreadable)');
  if (!env.NIMBO_CHROME) reasons.push('NIMBO_CHROME unset');
  if (!env.NIMBO_BRIDGE_STDIO_BIN) reasons.push('NIMBO_BRIDGE_STDIO_BIN unset');
  return { ok: reasons.length === 0, interfaces, reasons };
}

// ---------------------------------------------------------------- /proc socket audit
/** Kernel hex address: IPv4 is one little-endian word; IPv6 is four little-endian words. */
export function isLoopbackHex(hex) {
  const h = String(hex).toUpperCase();
  if (h.length === 8) return h.slice(6, 8) === '7F';
  if (h.length !== 32) return false;
  if (h === '00000000000000000000000001000000') return true; // ::1
  return h.startsWith('0000000000000000FFFF0000') && h.slice(30, 32) === '7F'; // ::ffff:127.x
}
export const isUnspecifiedHex = (hex) => /^0+$/.test(String(hex)) && (String(hex).length === 8 || String(hex).length === 32);

export function parseProcNet(text, proto) {
  const rows = [];
  for (const line of String(text).split('\n').slice(1)) {
    const f = line.trim().split(/\s+/);
    if (f.length < 10 || !f[1].includes(':')) continue;
    const addr = (a) => { const [hex, port] = a.split(':'); return { hex, port: parseInt(port, 16) }; };
    rows.push({ proto, local: addr(f[1]), remote: addr(f[2]), state: f[3], uid: f[7], inode: f[9] });
  }
  return rows;
}

/** null when allowed; otherwise the reason. Local must be loopback (a wildcard bind is reported). */
export function socketViolation(row) {
  if (!isLoopbackHex(row.local.hex)) return `local address ${row.local.hex} is not loopback`;
  if (!isLoopbackHex(row.remote.hex) && !isUnspecifiedHex(row.remote.hex)) return `remote address ${row.remote.hex} is not loopback`;
  return null;
}

const NET_TABLES = ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6'];

/** Strict shape check of a kernel net table: its header line, then only well-formed socket rows. */
export function procNetWellFormed(text) {
  const lines = String(text).split('\n');
  const head = lines[0].trim().split(/\s+/);
  if (head[0] !== 'sl' || head[1] !== 'local_address') return false;
  const addr = /^([0-9A-Fa-f]{8}|[0-9A-Fa-f]{32}):[0-9A-Fa-f]{4}$/;
  return lines.slice(1).filter((l) => l.trim()).every((l) => {
    const x = l.trim().split(/\s+/);
    return x.length >= 10 && /^\d+:$/.test(x[0]) && addr.test(x[1]) && addr.test(x[2]) && /^[0-9A-Fa-f]{2}$/.test(x[3]) && /^\d+$/.test(x[9]);
  });
}

/**
 * Lists a directory and runs `use(names)` while the listing handle is still open (when the fs has
 * opendirSync): a /proc/<self>/fd listing contains the reader's own descriptor, which a one-shot
 * readdir has already closed before its link can be read.
 */
function withListing(f, dir, use) {
  if (typeof f.opendirSync !== 'function') return use(f.readdirSync(dir));
  const h = f.opendirSync(dir);
  try {
    const names = [];
    for (let e; (e = h.readSync()) !== null;) names.push(e.name);
    return use(names);
  } finally { h.closeSync(); }
}

/** Sanitized error code only (never a message or path). */
const errCode = (e) => (e && typeof e.code === 'string' && /^E[A-Z0-9]+$/.test(e.code) ? e.code : 'ERROR');

/** `/proc/<pid>/stat` identity: { starttime, state }, or null when malformed. */
export function parseProcStat(text) {
  const s = String(text), r = s.lastIndexOf(')');
  if (r < 0) return null;
  const rest = s.slice(r + 1).trim().split(/\s+/);
  if (rest.length < 20 || !/^[A-Za-z]$/.test(rest[0]) || !/^\d+$/.test(rest[19])) return null;
  return { starttime: rest[19], state: rest[0] };
}

/**
 * Whole-thread-group exit proof for a leader seen in state Z/X: ONE bounded read each of
 * `status` (Threads), the `task` listing and `stat`. Proven only when Threads is 1, the task
 * listing is exactly the leader, and the same start time is still in state Z/X. Any missing,
 * unreadable, malformed or mismatched observation leaves it unproven (never retried).
 */
function threadGroupProof(f, base, name, starttime) {
  const reads = {};
  let threads = null, tasks = null, start = null, state = null;
  try {
    const m = /^Threads:\s*(\d+)\s*$/m.exec(f.readFileSync(path.join(base, 'status'), 'utf8'));
    if (m) { threads = Number(m[1]); reads.status = 'ok'; } else reads.status = 'MALFORMED';
  } catch (e) { reads.status = errCode(e); }
  try {
    const t = [...f.readdirSync(path.join(base, 'task'))].map(String);
    if (!t.length) reads.task = 'EMPTY';
    else if (!t.every((x) => /^\d+$/.test(x))) reads.task = 'MALFORMED';
    else { tasks = t.sort((a, b) => Number(a) - Number(b)); reads.task = 'ok'; }
  } catch (e) { reads.task = errCode(e); }
  try {
    const id = parseProcStat(f.readFileSync(path.join(base, 'stat'), 'utf8'));
    if (id) { start = id.starttime; state = id.state; reads.stat = 'ok'; } else reads.stat = 'MALFORMED';
  } catch (e) { reads.stat = errCode(e); }
  const proven = reads.status === 'ok' && reads.task === 'ok' && reads.stat === 'ok' && threads === 1
    && tasks.length === 1 && tasks[0] === name && start === starttime && /^[ZX]$/.test(state);
  return { reads, threads, tasks, starttime: start, state, proven };
}

/**
 * Every process visible in this pid namespace: fd socket inodes joined with its net tables.
 * Each process carries explicit sanitized read coverage (`reads`: input -> 'ok' | error code),
 * its `errors` and a stat identity read before and after; missing data is never empty data.
 */
export function auditProc({ procRoot = '/proc', fs: f = fs } = {}) {
  const processes = [], violations = [], incomplete = [];
  let names = [], enumCode = null;
  try { names = f.readdirSync(procRoot).filter((n) => /^\d+$/.test(n)); } catch (e) { enumCode = errCode(e); }
  if (!enumCode && !names.length) enumCode = 'EMPTY';
  const enumeration = { ok: !enumCode, code: enumCode, entries: names.length };
  for (const name of names) {
    const pid = Number(name), base = path.join(procRoot, name);
    const reads = {}, errors = [];
    const attempt = (input, fn) => {
      try { const v = fn(); reads[input] = 'ok'; return v; } catch (e) { fail(input, errCode(e)); return null; }
    };
    const fail = (input, code) => { reads[input] = code; errors.push({ input, code }); };
    const statText = attempt('stat', () => f.readFileSync(path.join(base, 'stat'), 'utf8'));
    const id0 = statText === null ? null : parseProcStat(statText);
    if (statText !== null && !id0) fail('stat', 'MALFORMED');
    const commText = attempt('comm', () => f.readFileSync(path.join(base, 'comm'), 'utf8'));
    const comm = (commText ?? '').trim();
    if (commText !== null && !comm) fail('comm', 'EMPTY');
    const fdDir = path.join(base, 'fd'), inodes = new Set();
    // An fd that disappears is closed only if one bounded re-listing of the fd directory
    // corroborates it; any other readlink failure is unread data.
    let fdsRead = 0, fdsClosed = 0, linkCode = null;
    const fdListed = attempt('fd', () => withListing(f, fdDir, (fdNames) => {
      fdsRead = fdNames.length;
      let relisted;
      for (const fd of fdNames) {
        let target = '';
        try { target = f.readlinkSync(path.join(fdDir, fd)); } catch (e) {
          const code = errCode(e);
          if (code === 'ENOENT') {
            if (relisted === undefined) { try { relisted = withListing(f, fdDir, (n) => n); } catch { relisted = null; } }
            if (relisted && !relisted.includes(fd)) { fdsClosed++; continue; }
          }
          linkCode = linkCode || code;
          continue;
        }
        const m = /^socket:\[(\d+)\]$/.exec(target);
        if (m) inodes.add(m[1]);
      }
      return true;
    }));
    if (linkCode) fail('fd_links', linkCode); else reads.fd_links = fdListed ? 'ok' : 'SKIPPED';
    const inet = [];
    const tables = {};
    for (const t of NET_TABLES) {
      const text = attempt('net/' + t, () => f.readFileSync(path.join(base, 'net', t), 'utf8'));
      tables[t] = text !== null;
      if (text === null) continue;
      if (!procNetWellFormed(text)) fail('net/' + t, 'MALFORMED'); // well-formed rows below are still audited
      for (const row of parseProcNet(text, t)) {
        if (!inodes.has(row.inode)) continue;
        const v = socketViolation(row);
        const rec = { proto: t, local: row.local, remote: row.remote, state: row.state, inode: row.inode };
        inet.push(rec);
        if (v) violations.push({ pid, comm, reason: v, ...rec });
      }
    }
    let id1 = null, recheck = 'ok';
    try { id1 = parseProcStat(f.readFileSync(path.join(base, 'stat'), 'utf8')); if (!id1) recheck = 'MALFORMED'; } catch (e) { recheck = errCode(e); }
    const identity = { starttime: id0 ? id0.starttime : null, state: (id1 || id0 || {}).state ?? null,
      stable: !!(id0 && id1 && id0.starttime === id1.starttime) };
    if (id0 && id1 && !identity.stable) fail('identity', 'CHANGED'); // pid reused mid-audit: unresolved
    else if (id0 && !id1) fail('identity', recheck); // identity not re-verified after the reads
    // Lifecycle: an error alone never proves exit. One bounded corroboration only: a stat recheck
    // ENOENT plus absence from one re-enumeration proves exit. A leader in state Z/X with the same
    // start time is NOT whole-group exit (other threads may still run and hold sockets), so it is
    // terminal only with the whole-thread-group proof below. Anything else with an error stays a
    // live, incomplete process.
    let status = 'live', lifecycle = null, threadGroup = null;
    if (errors.length && recheck === 'ENOENT') {
      let listed = null;
      try { listed = f.readdirSync(procRoot).includes(name); } catch { listed = null; }
      lifecycle = { stat_recheck: 'ENOENT', listed_on_reenumeration: listed };
      if (listed === false) status = 'vanished';
    } else if (errors.length && identity.stable && /^[ZX]$/.test(id1.state)) {
      threadGroup = threadGroupProof(f, base, name, identity.starttime);
      if (threadGroup.proven) {
        lifecycle = { stat_recheck: 'ok', state: id1.state };
        status = 'terminal';
      }
    }
    const matched = new Set(inet.map((s) => s.inode));
    processes.push({ pid, comm, status, identity, lifecycle, thread_group: threadGroup, reads, errors, fds_read: fdsRead, fds_closed: fdsClosed, inet_sockets: inet,
      non_inet_socket_inodes: [...inodes].filter((i) => !matched.has(i)).length, tables_read: tables });
    if (status === 'live' && errors.length) incomplete.push({ pid, comm, status, errors });
  }
  return { label: 'EVIDENCE (detection only; network-none namespace is the guarantee)', enumeration, processes, violations, incomplete,
    pass: enumeration.ok && processes.length > 0 && violations.length === 0 && incomplete.length === 0 };
}

// ---------------------------------------------------------------- CDP URL log
const URL_EVENTS = new Set(['Network.requestWillBeSent', 'Network.webSocketCreated']);
const LOGGED_EVENTS = new Set([...URL_EVENTS, 'Network.loadingFailed', 'Network.responseReceived',
  'Network.webSocketHandshakeResponseReceived', 'Network.webSocketFrameError']);

/** Masked, ordered network evidence from every attached CDP session. */
export class UrlLog {
  constructor() { this.entries = []; this.urls = new Map(); }
  record(method, params = {}, sessionId = null) {
    if (!LOGGED_EVENTS.has(method)) return;
    const raw = method === 'Network.webSocketCreated' ? params.url
      : method === 'Network.requestWillBeSent' ? params.request?.url
      : method === 'Network.responseReceived' ? params.response?.url : undefined;
    const key = `${sessionId}|${params.requestId}`;
    if (raw !== undefined) this.urls.set(key, { url: maskUrl(raw), cls: classifyUrl(raw) });
    const known = this.urls.get(key) || {};
    const e = { seq: this.entries.length, method, sessionId, requestId: params.requestId ?? null, url: known.url ?? null, url_class: known.cls ?? null };
    if (method === 'Network.requestWillBeSent') Object.assign(e, { http_method: params.request?.method ?? null, type: params.type ?? null,
      redirect_from: params.redirectResponse ? maskUrl(params.redirectResponse.url) : null });
    if (method === 'Network.loadingFailed') Object.assign(e, { errorText: params.errorText ?? null, canceled: !!params.canceled });
    if (method === 'Network.responseReceived') Object.assign(e, { status: params.response?.status ?? null, remoteIPAddress: params.response?.remoteIPAddress ?? null });
    if (method === 'Network.webSocketHandshakeResponseReceived') e.status = params.response?.status ?? null;
    this.entries.push(e);
  }
  count(method) { return this.entries.filter((e) => e.method === method).length; }
  /** URL-bearing events whose URL is not loopback/internal, plus responses from a non-loopback IP. */
  violations() {
    return this.entries.filter((e) => (URL_EVENTS.has(e.method) && e.url_class === 'external')
      || (e.method === 'Network.responseReceived' && e.remoteIPAddress && !loopbackHostname(e.remoteIPAddress.includes(':') ? `[${e.remoteIPAddress}]` : e.remoteIPAddress)));
  }
}

// ---------------------------------------------------------------- retained-evidence scan
const PATTERNS = [
  ['jwt', /eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*/g],
  ['google-api-key', /AIza[0-9A-Za-z_-]{35}/g],
  ['state-48hex', /(?<![0-9a-fA-F])[0-9a-f]{48}(?![0-9a-fA-F])/g],
  ['bearer', /Bearer\s+[A-Za-z0-9._-]{20,}/g],
];
/** Positions and kinds only; the matched text is never returned. */
export function scanForSecrets(text, markers = []) {
  const s = String(text), hits = [];
  for (const [kind, re] of PATTERNS) for (const m of s.matchAll(re)) hits.push({ kind, index: m.index, length: m[0].length });
  for (const marker of markers) {
    let i = -1;
    while (marker && (i = s.indexOf(marker, i + 1)) !== -1) hits.push({ kind: 'marker', index: i, length: marker.length });
  }
  return hits;
}
