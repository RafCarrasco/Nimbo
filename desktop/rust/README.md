# Nimbo checkpoint-2 native policy / Windows API probe

Status: checkpoint 2 INCOMPLETE. Native WGC/D3D11 pixel acquisition, a bounded read-only WebView2 shared-buffer sink and a desktop-owned canvas capture adapter now exist. An opt-in Windows integration test wires them into a real WebView2 and checks a real MediaStream/MediaStreamTrack and rendered video frames. That test has NOT been executed on Windows in this continuation; Windows-target source checks are not runtime proof. The default diagnostic executable still refuses remote navigation and does not open a GUI. No shipping entry point, installer/updater or packaging script is replaced.

## Portable checks

Rust 1.99.1, rustfmt and clippy are pinned by rust-toolchain.toml. Run from the repository root:

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked
    cargo +1.99.1 fmt --manifest-path desktop/rust/Cargo.toml --all -- --check
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets -- -D warnings
    cargo +1.99.1 check --manifest-path desktop/rust/Cargo.toml --locked --all-targets
    cargo +1.99.1 run --manifest-path desktop/rust/Cargo.toml --locked

The default diagnostic intentionally exits 2 (RETURN); that is not a media test failure or success. Portable tests exercise policy, lifetime/credits, real padded BGRA-to-RGBA byte conversion and expiring generation/session/frame/sequence-bound delivery. `node --test desktop/tests/capture-adapter.test.mjs` tests the actual adapter with explicitly labeled browser protocol doubles, NOT real MediaStream evidence. Windows-only ignored hardware tests perform actual acquisition/rendering and fail rather than substituting synthetic media.

For this coordinator's Linux environment only, the isolated profile HOME differs from the provisioned Rust home. Prefix commands with RUSTUP_HOME=/opt/data/home/.rustup CARGO_HOME=/opt/data/home/.cargo and add /opt/data/home/.cargo/bin to PATH. No global rustup/profile change is needed. Use the Node22 directory in /opt/data/cache/scratch/nimbo-community-node22-bin.txt for the unchanged Node suite/build.

## Windows source checks vs a real builder

Installed target libraries allow the following Linux metadata checks; they do NOT link an .exe or run COM/GUI/media:

    cargo +1.99.1 check --manifest-path desktop/rust/Cargo.toml --locked --all-targets --target x86_64-pc-windows-msvc
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings
    python3 desktop/rust/experiments/check-source-api.py

The experiment requires Python 3, that Rust target, cached locked dependencies (`cargo fetch --locked --manifest-path desktop/rust/Cargo.toml --target x86_64-pc-windows-msvc` if necessary), and an isolated TMPDIR. It checks positive/negative API compilation only, emitting exact compiler JSON. It never accesses production.

A real Windows x64 builder needs Rust 1.99.1 with x86_64-pc-windows-msvc, Visual Studio C++ Build Tools/MSVC linker and Windows SDK libraries, plus installed WebView2 Evergreen for the diagnostic runtime. Use an x64 Developer PowerShell/command prompt. Do not install or replace a production Nimbo application:

    rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked
    cargo +1.99.1 check --manifest-path desktop/rust/Cargo.toml --locked --all-targets
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets -- -D warnings
    cargo +1.99.1 build --manifest-path desktop/rust/Cargo.toml --locked --release --target x86_64-pc-windows-msvc
    .\desktop\rust\target\x86_64-pc-windows-msvc\release\nimbo-desktop.exe

Expected default diagnostic behavior: print installed WebView2 version if the loader succeeds, print configured hosted URL, refuse startup with RETURN and exit 2. Missing runtime/Win32 API failure exits 1. No login, production site, live updater, device capture or shell launch occurs. Prior Windows workflow run 42827156882 successfully linked/tested the preceding a5d88fc diagnostic; it does not validate the NEW capture code. Do not ask a user to validate native capture using the default diagnostic binary.

## Executable native capture experiment (interactive Windows ONLY)

Run the following from a clean checkout of the recorded capture-continuation commit, in an interactive Windows x64 desktop with hardware D3D11, WGC and WebView2. WGC HWND interop requires Windows 10 1903 or later; actual supported OS/runtime/device evidence is still required. These tests deliberately capture only their OWN native test window, never accept a script-supplied HWND, and do not open the system picker or imply full Nimbo picker parity.

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --test windows_capture -- --ignored --nocapture --test-threads=1

The test requires nonuniform actual window pixels, changes the source text, requires different captured pixels within five seconds and checks stop. It prints REAL_WGC_CHANGED_PIXELS only after those assertions pass.

For the complete local frame-to-track route, first create a NEW owned diagnostic scratch directory and set NIMBO_CAPTURE_TEST_DIR to its absolute path (PowerShell `$env:NIMBO_CAPTURE_TEST_DIR = 'C:\\path\\to\\owned-scratch'`), then:

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --test windows_webview_capture -- --ignored --nocapture --test-threads=1

This opens separate source/receiver windows, starts an ephemeral 127.0.0.1-only fixed-page server and uses an isolated WebView profile beneath that scratch directory. It queries the actual native main-frame ID after navigation, denies subsequent navigation/subframe navigation/popups/permissions/system capture, and grants NO WebMessageReceived/native command handler. Test controls/ACKs are read back by host ExecuteScript in the fixed main document. The same production-candidate adapter is invoked with a test-local control callback, not fake media. It checks actual MediaStream and MediaStreamTrack instances, live state, at least three requestVideoFrameCallback notifications, then ended state after stop. REAL_WEBVIEW_MEDIASTREAM is printed only after live-render assertions. Stop/controller/window/server/apartment cleanup is scoped; retain the printed isolated profile for diagnostics and remove ONLY that profile after its WebView browser has exited. The test never logs credentials or contacts Firebase. Enforce an external test-process timeout if COM/runtime callbacks hang.

This local test is the next demonstrable boundary, NOT unchanged-community WebRTC publication or an independent receiver. It has not run in this Linux continuation. Normal Windows `cargo test` skips both ignored hardware tests; CI green with skips does not close checkpoint 2. The existing artifact-only workflow is unchanged and does not package these test executables. Coordinator must arrange these exact commands on an interactive source checkout; running the old diagnostic artifact cannot exercise them.

Diagnostic limitations are explicit: own-process window only; fixed size (resize/device/source loss closes instead of leaking stale pixels); SDR BGRA readback/opaque RGBA; two outstanding mappings; fresh immutable mapping per frame; 2-second ACK expiry; publication interval at least 17 ms; no audio/game/login implementation. The adapter accepts ideal constraints and max frameRate >=60 only, rejects narrower max/min/exact/audio, revokes ALL clones when any clone stops, and disposes on pagehide/missing frames. Full constraints/settings/ended semantics, 30 fps mode, performance/HDR/minimized/game privacy, picker parity, main-host IPC, login, independent WebRTC and resource-cycle evidence remain gates. No prototype behavior is installed into the hosted app or declared parity.

## Commit-bound evidence checklist

Before recording ANY Windows result, include `git rev-parse HEAD`, `git status --short`, `rustc +1.99.1 --version`, Windows edition/build, architecture, WebView2 runtime, builder tool/SDK versions, exact command, exit status and output. Record artifact SHA-256 and bytes; size is informational. This worker does not push/dispatch; coordinator owns those authorized actions. Release/deploy/merge remain forbidden. Existing artifact-only workflow bytes remain unchanged.

Current evidence checklist (prior diagnostic linking is established; NEW capture runtime is pending):

- [ ] Windows link/tests/clippy for this capture-continuation commit (prior a5d88fc Windows link passed).
- [ ] Real host integration: create a blank WebView, install DenyGuards before navigation, missing-interface/registration/deny-failure rollback and controller closure. Current guard library code is only cross-typechecked.
- [ ] Execute both real Windows hardware tests above; record actual native pixels and WebView MediaStream/render/stop results. Implementation exists; runtime proof remains pending.
- [ ] Unchanged branch source in isolated emulator mode publishes that REAL track to an independent browser; remote hosted release configuration stays unchanged. No fakeMedia=1/native-evidence substitution.
- [ ] Choose two distinct windows/monitors and observe the chosen source, constraints at 720/1080 and 30/60 fps, timestamps/receiver dimensions/frame counts/latency and camera independence. No inferred success from a local preview.
- [ ] Cancel/reselect; stop/native-ended/close/reload/navigation; stale session/ACK; clone/lifetime cleanup and bounded resources after repeated cycles.
- [ ] Denied foreign/same-origin iframe, opaque/unknown frame, forged native command/source/generation and unsupported interface paths, observed in the actual native host.
- [ ] Source/audio-intent/game PID binding retained for later ports; no claim of implemented app audio or game focus privacy yet. Full app-only/no-Nimbo audio, Alt+Tab/game exit, picker parity, installer/updater and session persistence are still mandatory later gates.

Only real Windows capture evidence can close checkpoint 2. Portable passing tests, a source check, an installed runtime or a linked artifact cannot. Stop here; do not implement checkpoints 3–8 under this approval.
