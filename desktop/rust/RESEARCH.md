# Checkpoint 2: API investigation — RETURN, not capture parity

This independent crate is a native feasibility experiment, NOT a replacement application, installer or full custom picker. The original investigation below describes the preceding metadata-only checkpoint. The capture continuation now implements WGC/D3D11 acquisition, actual shared-buffer posting, a canvas MediaStream adapter and a fully wired opt-in Windows local integration test; see the final section for precisely what is implemented versus unexecuted. No web files, Electron shipping entry points or workflows are changed. All later feature ports remain gated.

## Documented incompatibility reproduced

Electron currently calls `callback({ video: selected.source })` after its custom picker (`../main.cjs:679–693`). WebView2's `ICoreWebView2ScreenCaptureStartingEventArgs` has only Cancel, Handled, OriginalSourceFrameInfo and GetDeferral (plus the inherited IUnknown methods). Cancel denies the call. Handled only stops propagation between frame/core event handlers; it does NOT mean "use my selected source". Deferral delays the decision to display the WebView2 system UI. None accepts an HWND, GraphicsCaptureItem, video track or custom source identifier.

Primary API references inspected:

- https://learn.microsoft.com/en-us/microsoft-edge/webview2/webview2-api-reference
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2screencapturestartingeventargs
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2_27
- Pinned generated source: `webview2-com-sys` 0.39.1, `src/bindings.rs`, `impl ICoreWebView2ScreenCaptureStartingEventArgs` and its vtable. Cargo.lock fixes the registry checksum. This is a third-party binding of the primary COM surface, not a Microsoft SDK guarantee by itself.

`python3 desktop/rust/experiments/check-source-api.py` builds a disposable Windows-target crate twice, using this lockfile. Positive control `SetCancel(true)` must typecheck. The deliberately hypothetical `SetSelectedSource(1)` must fail with E0599 for that method, NOT with dependency, compiler or linker errors. The hypothetical method is never used in the implementation; its absence supports the documented incompatibility, not a claim to have exhaustively disproven all adapters. Both fixtures are removed. No Windows runtime is involved.

The old Rust shell was consulted read-only. Only the pure navigation/window concepts were adapted to current identity, origin, permissions and frame/generation requirements. No old workspace or GoLive config was imported. Its unhandled registration errors and system-picker-only approach were not reused.

## Original investigation (before capture continuation)

The most concrete supported building blocks found are:

1. `IGraphicsCaptureItemInterop::CreateForWindow(HWND, ...)` / CreateForMonitor binds native Windows Graphics Capture to a host-selected source, unlike ScreenCaptureStarting. A native picker must validate/own handles and PIDs, including HWND reuse, session expiry and process lifetime. This is not the WebView2 system picker. Native support/device/OS/consent failures must be visible.
2. A Direct3D11CaptureFramePool supplies actual window/display frames. Its ContentSize must bound copied pixels (padding can be undefined), frames/surfaces cannot outlive checkout, resize/device-loss must recreate safely, BGRA/HDR need explicit conversion. The native producer is NOT implemented here.
3. WebView2 Environment12.CreateSharedBuffer and CoreWebView2_17.PostSharedBufferToScript can share native memory read-only to the MAIN FRAME. This transports an ArrayBuffer, NOT a MediaStream. `candidate_interfaces` typechecks the required QueryInterface calls; it is not exercised on a WebView. Never use frame-post APIs for this route. JSON may contain bounded control/session/slot information, never unbounded video payloads. Read-only mapping writes crash the renderer. Closing native memory does NOT revoke the existing script view: both sides must release; stop producing before navigation. Do not overwrite a slot until its matching session/sequence ACK. Timeouts must close the session rather than queue frames indefinitely.
4. A desktop initialization script could replace only getDisplayMedia in the authorized top document, copy validated live pixels to an origin-clean canvas, and return canvas.captureStream's actual MediaStream/video track to the unchanged caller. No such script is shipped here: a canvas demo or a track-shaped JS object would not establish native compatibility. Tauri's remote capability declarations alone do not prove frame attribution. Tauri integration/ACL configuration is deferred rather than adding an unused broad capability.

Primary sources:

- https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow
- https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment12
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2_17
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2sharedbuffer
- https://w3c.github.io/mediacapture-fromelement/ (draft; engine behavior still requires testing)
- https://v2.tauri.app/reference/acl/capability/

Additional browser implementation reference: https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/captureStream . No browser synthetic source was used as native evidence. Two initially attempted Microsoft/W3C URLs returned 404; the valid links above were retrieved instead. No undocumented source-selection API is assumed.

## Why the candidate does not yet pass checkpoint 2

There is no native producer, native custom picker, shared-memory transport, injected getDisplayMedia bridge or independent browser receive evidence in this artifact. Linux cannot establish Windows engine behavior. A native Windows implementation and interactive experiment are required before selecting this route as viable; this research does NOT prove it impossible either.

The unchanged consumer at `src/live/capture.ts:90–103` awaits setCaptureAudio, requests real getDisplayMedia video with ideal width/height and ideal/max frameRate, adds native audio then attaches game monitoring. A compatible adapter must preserve real MediaStream/MediaStreamTrack methods, applyConstraints/getSettings/capabilities semantics, contentHint, WebRTC publication, getUserMedia camera independence, and rejection/cancel behavior. `stop()` ends a track without firing its own ended event: native resources must still stop; native source closure must produce appropriate termination. Clones/peer references must not leak capture after cancellation. Native request identity cannot come from JS-supplied URL/frame flags.

Native frame policy must reject unknown/zero frame IDs, same-origin iframes, opaque frames, foreign origins/windows and stale documents. FrameInfo2's documentation warns some frame IDs are zero/stale; reject these rather than infer top-level identity from URL alone. See https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2frameinfo2 . The current native guard denies EVERY permission/capture request, so an incomplete attribution adapter never grants access. Portable policy tests model future authorized decisions; they are not wired as production permission grants.

Background/minimized performance, 30/60 fps, multi-monitor/DPI, minimized/fullscreen game source binding, Alt+Tab privacy and app-only/exclude-Nimbo audio remain mandatory. Metadata-only Binding tests do not implement those features. The model's 3840×2160/60 experiment ceiling is not a product resolution restriction. Its two-slot credits are not actual memory/IPC/native-media evidence.

No generic-picker fallback, fake/prerecorded capture, JSON video pump, cookie/token migration, provider changes, updater replacement, portable-only cutover or web edits are authorized substitutes. Electron remains the shipping product. Checkpoint 2 decision remains RETURN.

## Capture continuation: implemented route, Windows proof pending

Current primary references were retrieved again before implementation (versioned WebView2 pages used after unversioned retrieval timed out):

- https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture — actual ContentSize must bound readback; BGRA SDR is not HDR parity.
- https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow — HWND source, Windows 10 1903 minimum for interop.
- https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded — removes DispatcherQueue requirement; internal capture worker, owner-thread polling for this experiment.
- https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.trygetnextframe — empty pool returns null. Read the documented ABI HRESULT and pointer separately so S_OK/null is no-frame but a genuine E_POINTER or other failing HRESULT still stops capture; do not treat every projection E_POINTER as benign.
- https://developer.mozilla.org/en-US/docs/Web/API/HTMLCanvasElement/captureStream and https://developer.mozilla.org/en-US/docs/Web/API/CanvasCaptureMediaStreamTrack/requestFrame — actual stream/manual frame APIs; requestFrame compatibility is limited and checked at runtime, not assumed from an installed WebView version.
- https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map — staging Map/Unmap lifetime, no mapped pointer escapes.
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2_17?view=webview2-1.0.3537.50 — MAIN-frame posting, read-only mapping, getBuffer/additionalData/source, releaseBuffer, repeated posting creates separate views; native close cannot revoke retained views.
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2sharedbuffer?view=webview2-1.0.3537.50 — Buffer/Size/Close backed by shared memory.
- https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2_20?view=webview2-1.0.3537.50 — actual native main FrameId, possibly zero before ContentLoading. The local integration test queries after load and rejects zero; it never invents a top-frame ID.
- Pinned windows 0.62.2 generated Microsoft windows-rs source for WGC, D3D11, DXGI, WinRT interop and Win32 window APIs; pinned webview2-com-sys 0.39.1 signatures for shared memory, FrameId, environment/controller creation, script execution and denial events were inspected before use. No hypothetical source-setter/import-track API was introduced.

Cargo changes only enable features on the existing exact windows pin: Foundation/WGC/DirectX/D3D11/DXGI/GDI/WinRT/window APIs needed for these actual calls and diagnostic windows. No new crate/version or lockfile change is needed; `--locked` checks remain authoritative. No root dependency graph changes.

Implemented in src/frame_route/windows.rs: validate live HWND belongs to this diagnostic process (caller also owns its lifetime), check WGC support, CreateForWindow, hardware D3D11 BGRA device, DXGI-to-WinRT device, two-frame free-threaded pool, capture session, source-closed observation, actual frame ContentSize/texture validation, staging GPU copy/Map, bounded padded BGRA-to-opaque-RGBA conversion, unconditional Unmap/frame release and stop on errors/resize/identity loss. There is deliberately no monitor or arbitrary external-process capture constructor. No generic picker is substituted. Native session/pool/interface/registration failures propagate; drop performs scoped cleanup.

Implemented transport: SharedFrameSink owns ONE capture source for its entire lifetime, allocates at most two outstanding read-only shared buffers, copies actual RGBA bytes then posts only bounded numeric metadata JSON plus shared memory. Pump is a real WGC-to-WebView step. No pixels/base64 in JSON. Expired or regressing host-monotonic time closes the route/producer. Session/generation/native-main-frame/slot/sequence authorization is portable-tested; unknown identity is denial. ACK validation cannot release another sequence/session. Fresh backing memory is used for EVERY frame, rather than overwriting an ACKed mapping: closing a native buffer does not revoke a maliciously retained page view, so slot reuse must not expose later pixels through that view. A hostile trusted page can retain mappings/allocate its own memory; credits bound HOST-owned outstanding mappings, not total arbitrary renderer allocations. Two GPU-pool surfaces, one staging/readback conversion and two outstanding mappings are bounded; this allocation-heavy design has no performance proof yet.

Implemented JS: an explicit factory (not automatically injected into remote content) uses actual canvas.captureStream(0)/CanvasCaptureMediaStreamTrack.requestFrame, returns the ACTUAL stream after a validated native frame, copies read-only bytes before release and ACK, rejects stale/wrong-size/format/source/sequence data, stops all diagnostic clones and restores getDisplayMedia on dispose/pagehide/timeouts. Unsupported audio, mandatory/narrower-max constraints reject instead of reporting successful unavailable functionality. Only the local ignored integration fixture currently installs this factory; there is NO remote IPC dispatcher and release navigation is still closed. This is a diagnostic lifecycle policy, not complete Web API parity (settings/applyConstraints/native-ended/clone semantics remain unresolved).

Executable evidence boundaries:

- Portable Rust tests process concrete byte arrays, padding, malformed layouts, credits, stale identity and expiry; they do not call WGC.
- Node tests evaluate the actual factory against explicitly labeled canvas/track protocol doubles; never media proof.
- cfg(windows) source checks compile the real producer, sink and two native test programs, including COM APIs. No runtime result is inferred from compilation.
- `tests/windows_capture.rs` ignored test creates a real owned window, captures nonuniform pixels, changes its text and requires different captured pixels, then stops.
- `tests/windows_webview_capture.rs` ignored test creates a real source window and a real WebView2 receiver of a fixed isolated loopback document, queries its native FrameId, installs checked deny hooks, pumps actual capture into shared memory, calls the real adapter/getDisplayMedia and requires actual MediaStream/MediaStreamTrack instances, live state and multiple decoded video callbacks, then stop/ended. There are no page-to-native command handlers; ACKs/control are test-local JS readback via native ExecuteScript. The native-owned single document is not an authorization shortcut for remote IPC. Both tests are UNEXECUTED on Windows in this continuation; README gives exact commands.

Prior workflow 42827156882 at a5d88fc linked/tested the earlier diagnostic, resolving the former linker blocker; it did not exercise these new layers. Coordinator owns the next push/dispatch, keeping the artifact-only workflow unchanged. Checkpoint 2 remains INCOMPLETE until Windows capture/MediaStream evidence and unchanged-community publication to an independent browser; audio/game/login/picker/installer are not faked or dropped.
