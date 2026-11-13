// Loopback-only static SPA server for the S2-integration browser tiers (container only). Serves
// the emulator-mode production bundle exactly as built (no rewriting): files under `root`,
// `index.html` for extension-less routes such as /desktop-login. GET/HEAD only; never binds a
// non-loopback address; request log keeps masked URLs (the login state is never recorded).
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { maskUrl } from './offline-evidence.mjs';

const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.json': 'application/json', '.png': 'image/png', '.svg': 'image/svg+xml', '.ico': 'image/x-icon', '.map': 'application/json',
  '.webmanifest': 'application/manifest+json', '.txt': 'text/plain; charset=utf-8', '.woff2': 'font/woff2' };

export function assertLoopbackBind(host) {
  if (host !== '127.0.0.1') throw new Error(`static server binds loopback 127.0.0.1 only, got ${JSON.stringify(host)}`);
  return host;
}

/** Absolute file path inside `root`, `root/index.html` for routes, or null (rejected). */
export function resolveStaticPath(root, rawUrl) {
  const pathname = String(rawUrl).split(/[?#]/)[0];
  let decoded;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return null;
  }
  if (!decoded.startsWith('/') || decoded.includes('\0') || decoded.includes('\\')) return null;
  if (decoded.split('/').some((seg) => seg === '..' || seg === '.')) return null;
  const base = path.resolve(root);
  const full = path.resolve(base, '.' + decoded);
  if (full !== base && !full.startsWith(base + path.sep)) return null;
  if (decoded === '/' || path.extname(decoded) === '') return path.join(base, 'index.html');
  return full;
}

export async function startStaticServer({ root, host = '127.0.0.1', port = 0 }) {
  assertLoopbackBind(host);
  const realRoot = fs.realpathSync(root);
  const requests = [];
  const server = http.createServer((req, res) => {
    const rec = { method: req.method, url: maskUrl(`http://${host}${req.url}`), status: 0 };
    requests.push(rec);
    const send = (status, body, type = 'text/plain; charset=utf-8') => {
      rec.status = status;
      res.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store', 'Content-Length': Buffer.byteLength(body) });
      res.end(req.method === 'HEAD' ? undefined : body);
    };
    if (req.method !== 'GET' && req.method !== 'HEAD') return send(405, 'method not allowed');
    const file = resolveStaticPath(realRoot, req.url);
    if (!file) return send(400, 'bad path');
    let real;
    try {
      real = fs.realpathSync(file);
    } catch {
      return send(404, 'not found');
    }
    if (real !== realRoot && !real.startsWith(realRoot + path.sep)) return send(403, 'outside root');
    if (!fs.statSync(real).isFile()) return send(404, 'not found');
    return send(200, fs.readFileSync(real), TYPES[path.extname(real).toLowerCase()] || 'application/octet-stream');
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, resolve);
  });
  const address = server.address();
  if (address.address !== '127.0.0.1') {
    server.close();
    throw new Error('static server ended up on a non-loopback address');
  }
  return {
    port: address.port,
    origin: `http://127.0.0.1:${address.port}`,
    requests,
    close: () => new Promise((resolve) => { server.closeAllConnections?.(); server.close(() => resolve()); }),
  };
}
