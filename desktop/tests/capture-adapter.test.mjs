// Protocol test doubles, NOT browser/native MediaStream proof.
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
const source = readFileSync(new URL('../rust/bridge/capture.js', import.meta.url), 'utf8')

function harness({ subframe = false, frameRate } = {}) {
  const listeners = new Map(), messages = [], released = [], pixels = [], tracks = []
  class Track extends EventTarget {
    constructor() { super(); this.kind = 'video'; this.readyState = 'live'; tracks.push(this) }
    stop() { this.readyState = 'ended' }
    clone() { return new Track() }
    requestFrame() { this.frames = (this.frames || 0) + 1 }
  }
  const original = () => { throw Error('system picker must not run') }
  const mediaDevices = { getDisplayMedia: original }
  const webview = {
    addEventListener: (n, f) => listeners.set(n, f), removeEventListener: (n) => listeners.delete(n),
    postMessage: m => messages.push(m), releaseBuffer: b => released.push(b),
  }
  const window = { top: null, addEventListener: (n,f) => listeners.set(n,f), removeEventListener: n => listeners.delete(n) }
  window.top = subframe ? {} : window
  const canvas = { getContext: () => ({ putImageData: image => pixels.push([...image.data]) }), captureStream: fps => { assert.equal(fps, 0); const track = new Track(); return { getVideoTracks: () => [track], getTracks: () => [track] } } }
  const context = vm.createContext({ window, navigator: { mediaDevices }, document: { createElement: n => { assert.equal(n, 'canvas'); return canvas } }, DOMException, Event, Uint8ClampedArray, ImageData: class { constructor(data) { this.data = data } }, setTimeout, clearTimeout })
  const install = vm.runInContext(source, context)
  const dispose = install(webview, { generation: 3, session: 17, width: 2, height: 1, frameRate })
  const frame = (overrides = {}, bytes = new Uint8Array([1,2,3,255,4,5,6,255]).buffer) => {
    listeners.get('sharedbufferreceived')({ source: webview, getBuffer: () => bytes, additionalData: { kind: 'nimbo-frame', format: 'RGBA8', generation: 3, session: 17, slot: 0, sequence: 1, width: 2, height: 1, ...overrides } })
    return bytes
  }
  return { mediaDevices, original, dispose, frame, pixels, messages, released, tracks, listeners }
}

test('unsupported native frame-rate configuration is refused at installation', () => {
  for (const frameRate of [0, 1, 29, 31, 59, 61, 30.5, NaN, Infinity, '30', null]) {
    assert.throws(() => harness({ frameRate }), { name: 'SecurityError' })
  }
})

test('a native-configured 30 fps route accepts the unchanged consumer constraint shape', async () => {
  const h = harness({ frameRate: 30 })
  try {
    const pending = h.mediaDevices.getDisplayMedia({ video: {
      width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 30, max: 30 },
    }, audio: false })
    h.frame()
    const stream = await pending
    assert.equal(stream.getVideoTracks()[0], h.tracks[0])
    assert.equal(h.tracks[0].frames, 1)
  } finally { h.dispose() }
})

test('first validated frame resolves stream, copies before release/ACK, and never uses picker', async () => {
  const h = harness()
  const pending = h.mediaDevices.getDisplayMedia({ video: { width: { ideal: 1920 }, frameRate: { max: 60 } }, audio: false })
  const bytes = h.frame()
  const stream = await pending
  assert.equal(stream.getVideoTracks()[0], h.tracks[0])
  assert.deepEqual(h.pixels, [[1,2,3,255,4,5,6,255]])
  assert.equal(h.released[0], bytes)
  assert.equal(h.messages.at(-1).kind, 'nimbo-frame-ack')
  assert.equal(h.tracks[0].frames, 1)
  h.dispose()
  assert.equal(h.mediaDevices.getDisplayMedia, h.original)
  assert.equal(h.tracks[0].readyState, 'ended')
  assert.equal(h.listeners.size, 0)
})

test('stale generation/session/sequence and wrong layout cannot draw', async () => {
  const h = harness()
  const pending = h.mediaDevices.getDisplayMedia({ video: true })
  h.frame({ generation: 2 }); h.frame({ session: 18 }); h.frame({ width: 3 })
  assert.equal(h.pixels.length, 0)
  h.frame(); await pending
  h.frame(); h.frame({ sequence: 0 }); h.frame({ sequence: 2 }, new ArrayBuffer(1))
  assert.equal(h.pixels.length, 1)
  assert.equal(h.released.length, 7)
  h.dispose()
})

test('unsupported audio/mandatory constraints reject; stop and clones release host exactly once', async () => {
  const h = harness()
  await assert.rejects(h.mediaDevices.getDisplayMedia({ video: true, audio: true }), { name: 'NotSupportedError' })
  await assert.rejects(h.mediaDevices.getDisplayMedia({ video: { width: { exact: 3 } } }), { name: 'NotSupportedError' })
  await assert.rejects(h.mediaDevices.getDisplayMedia({ video: { frameRate: { max: 30 } } }), { name: 'NotSupportedError' })
  const pending = h.mediaDevices.getDisplayMedia({ video: true })
  h.frame(); const stream = await pending
  const clone = stream.getVideoTracks()[0].clone()
  stream.getVideoTracks()[0].stop()
  assert.equal(clone.readyState, 'ended', 'diagnostic stop revokes all clones')
  assert.equal(h.messages.filter(m => m.kind === 'nimbo-capture-stop').length, 1)
  h.dispose()
  assert.equal(h.messages.filter(m => m.kind === 'nimbo-capture-stop').length, 1)
})

test('pagehide rejects pending capture and cleans up, without a fake stream', async () => {
  const h = harness()
  const pending = h.mediaDevices.getDisplayMedia({ video: true })
  h.listeners.get('pagehide')()
  await assert.rejects(pending, { name: 'AbortError' })
  assert.equal(h.mediaDevices.getDisplayMedia, h.original)
  assert.equal(h.listeners.size, 0)
})

test('same-origin subframes cannot install capture and missing native frames time out', async () => {
  assert.throws(() => harness({ subframe: true }), { name: 'SecurityError' })
  const h = harness()
  await assert.rejects(h.mediaDevices.getDisplayMedia({ video: true }), { name: 'AbortError' })
  assert.equal(h.tracks.length, 0, 'no track-shaped success without a frame')
  assert.equal(h.listeners.size, 0)
})
