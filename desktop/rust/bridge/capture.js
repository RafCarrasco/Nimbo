// Desktop-owned feasibility adapter FUNCTION, not automatically installed in remote pages.
// Host must validate document/frame ownership before installation and every IPC operation.
(function installNativeCapture(webview, config, control) {
  'use strict'
  control ??= message => webview.postMessage(message)
  if (window.top !== window || !webview ||
      !['generation', 'session', 'width', 'height'].every(k => Number.isSafeInteger(config[k]) && config[k] > 0) ||
      config.width > 3840 || config.height > 2160) throw new DOMException('Unattributed capture', 'SecurityError')
  const devices = navigator.mediaDevices
  const original = devices.getDisplayMedia
  let pending, stream, canvas, context, timer, closed = false, sequence = 0
  const stops = new Map()
  const send = (kind, extra = {}) => control({ kind, generation: config.generation, session: config.session, ...extra })
  function dispose() {
    if (closed) return
    closed = true
    clearTimeout(timer)
    for (const stop of stops.values()) stop()
    stops.clear()
    pending?.reject(new DOMException('Capture closed', 'AbortError'))
    pending = undefined
    stream = canvas = context = undefined
    webview.removeEventListener('sharedbufferreceived', receive)
    window.removeEventListener('pagehide', dispose)
    if (devices.getDisplayMedia === getDisplayMedia) devices.getDisplayMedia = original
    send('nimbo-capture-stop')
  }
  function own(track) {
    const stop = track.stop.bind(track), clone = track.clone.bind(track)
    stops.set(track, stop)
    track.stop = dispose // feasibility policy: stopping any clone revokes the entire source
    track.clone = () => {
      if (closed) throw new DOMException('Capture closed', 'InvalidStateError')
      const copy = clone(); own(copy); return copy
    }
    track.addEventListener('ended', dispose, { once: true })
  }
  async function getDisplayMedia(options = {}) {
    if (closed || pending || stream) throw new DOMException('Capture already used', 'InvalidStateError')
    if (!options.video || options.audio ||
        (typeof options.video === 'object' && Object.entries(options.video).some(([key, v]) =>
          v && typeof v === 'object' && ('exact' in v || 'min' in v ||
            ('max' in v && (key !== 'frameRate' || !Number.isFinite(v.max) || v.max < 60)))))) {
      throw new DOMException('Only diagnostic video with ideal constraints is available', 'NotSupportedError')
    }
    // No native picker fallback. The host has already chosen its OWN diagnostic window.
    canvas = document.createElement('canvas')
    canvas.width = config.width; canvas.height = config.height
    context = canvas.getContext('2d', { alpha: false })
    if (!context || typeof canvas.captureStream !== 'function') {
      dispose(); throw new DOMException('Canvas capture unavailable', 'NotSupportedError')
    }
    const result = new Promise((resolve, reject) => { pending = { resolve, reject } })
    timer = setTimeout(dispose, 2000)
    send('nimbo-capture-ready')
    return result
  }
  function receive(event) {
    let buffer, accepted = false, metadata
    try {
      buffer = event.getBuffer()
      metadata = event.additionalData
      if (closed || !pending && !stream || event.source !== webview ||
          metadata?.kind !== 'nimbo-frame' || metadata.format !== 'RGBA8' ||
          metadata.generation !== config.generation || metadata.session !== config.session ||
          metadata.width !== config.width || metadata.height !== config.height ||
          !Number.isSafeInteger(metadata.sequence) || metadata.sequence <= sequence ||
          !Number.isInteger(metadata.slot) || metadata.slot < 0 || metadata.slot > 1 ||
          buffer.byteLength !== config.width * config.height * 4) return
      // Shared read-only bytes are COPIED before release. Never write into the shared mapping.
      const copy = new Uint8ClampedArray(buffer.byteLength)
      copy.set(new Uint8ClampedArray(buffer))
      context.putImageData(new ImageData(copy, config.width, config.height), 0, 0)
      if (!stream) {
        stream = canvas.captureStream(0)
        const [track] = stream.getVideoTracks()
        if (!track || typeof track.requestFrame !== 'function') throw new Error('Manual canvas frames unavailable')
        own(track)
      }
      stream.getVideoTracks()[0].requestFrame()
      sequence = metadata.sequence
      accepted = true
      clearTimeout(timer)
      timer = setTimeout(dispose, 2000)
      if (pending) { pending.resolve(stream); pending = undefined }
    } catch (_) {
      dispose()
    } finally {
      if (buffer) webview.releaseBuffer(buffer)
      if (accepted) send('nimbo-frame-ack', { slot: metadata.slot, sequence: metadata.sequence })
    }
  }
  webview.addEventListener('sharedbufferreceived', receive)
  window.addEventListener('pagehide', dispose, { once: true })
  devices.getDisplayMedia = getDisplayMedia
  return dispose
})
