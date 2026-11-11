# Nimbo Rust desktop host (checkpoint 1 shell) and retained capture groundwork

Status: checkpoint 1 of the full-parity plan is IMPLEMENTED LOCALLY, NOT VERIFIED ON WINDOWS. `nimbo-desktop.exe` now creates a real Win32 window with a WebView2 controller that opens https://golive-ad118.web.app/ with Electron's window geometry/identity, navigation/popup/permission policy, `Nimbo/<version>` user agent, F5/Ctrl+R reload and single-instance focus. It installs NO `nimboDesktop` bridge (the page sees a plain browser), performs no login, does not register or handle `golive://` (a first `golive://` argument is only recognized and logged as a fixed code), and has no capture, audio, game watch, updater or installer. `getDisplayMedia()` is natively cancelled. The Electron app remains the only distributed desktop app.

This host has only been type-checked/linted for `x86_64-pc-windows-msvc` from Linux. Linking, GUI behavior, the interactive host test and the Electron parity matrix (`desktop/tests/e2e/shell-parity.md`) have NOT been executed. The capture groundwork below (checkpoint-2 feasibility of the previous plan) is unchanged and still unproven at runtime; it is not wired into the shell.

## Portable checks

Rust 1.99.1, rustfmt and clippy are pinned by rust-toolchain.toml. Run from the repository root:

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host
    cargo +1.99.1 fmt --manifest-path desktop/rust/Cargo.toml --all -- --check
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets -- -D warnings
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets --features test-host -- -D warnings
    node --test desktop/tests/release-guard.test.mjs

On a non-Windows platform the executable only prints that it requires Windows and exits 2. Portable tests exercise the shell policy (navigation/popup/permission matrix, accelerators, UA, browser switches, geometry, title, argv classification, single-instance identity/message, startup guards) plus the retained capture lifetime/credits/conversion logic. `node --test desktop/tests/capture-adapter.test.mjs` tests the capture adapter with explicitly labeled browser protocol doubles, NOT real MediaStream evidence.

`test-host` is a debug-only feature for the interactive host test: the executable then requires `--test-host-url http://127.0.0.1:<port>/ --test-host-dir <absolute owned dir>`, keeps its WebView2 profile and `events.log` under that directory and records external opens instead of calling ShellExecute. It cannot be built in a release profile: build.rs rejects `PROFILE=release` (even with debug assertions forced on) and a `compile_error!` rejects any build without debug assertions. `release-guard.test.mjs` checks both, plus positive controls.

For this coordinator's Linux environment only, prefix commands with RUSTUP_HOME=/opt/data/home/.rustup CARGO_HOME=/opt/data/home/.cargo and put /opt/data/home/.cargo/bin and the pinned Node 22 directory first on PATH. No global rustup/profile change is needed.

## Windows source checks vs a real builder

Installed target libraries allow the following Linux metadata checks; they do NOT link an .exe or run COM/GUI/media. On a non-Windows host build.rs cannot compile the icon resource and says so with a cargo warning; such output is never a release artifact:

    cargo +1.99.1 check --manifest-path desktop/rust/Cargo.toml --locked --all-targets --target x86_64-pc-windows-msvc
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets --target x86_64-pc-windows-msvc --features test-host -- -D warnings
    python3 desktop/rust/experiments/check-source-api.py

The experiment requires Python 3, that Rust target, cached locked dependencies (`cargo fetch --locked --manifest-path desktop/rust/Cargo.toml --target x86_64-pc-windows-msvc` if necessary), and an isolated TMPDIR. It checks positive/negative API compilation only, emitting exact compiler JSON. It never accesses production.

A real Windows x64 builder needs Rust 1.99.1 with x86_64-pc-windows-msvc, Visual Studio C++ Build Tools/MSVC linker, the Windows SDK (rc.exe for the icon resource; a Windows-host build fails if the icon cannot be embedded) and the WebView2 Evergreen Runtime to run it. Use an x64 Developer PowerShell. Do not install or replace a production Nimbo application:

    rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host
    cargo +1.99.1 clippy --manifest-path desktop/rust/Cargo.toml --locked --all-targets -- -D warnings
    cargo +1.99.1 build --manifest-path desktop/rust/Cargo.toml --locked --release
    .\desktop\rust\target\release\nimbo-desktop.exe

Expected release behavior (to be OBSERVED, not yet observed): a "Nimbo" window opens the hosted login screen and the title becomes "Nimbo 0.4.1". Do not sign in. The WebView2 profile lives in `%LOCALAPPDATA%\Nimbo\WebView2`. If WebView2 cannot start, a native error dialog appears and the process exits 1. Release builds are GUI-subsystem executables and print nothing visible.

Interactive host test (test-host build, 127.0.0.1 fixture only, no hosted site, no login, no ShellExecute):

    $env:NIMBO_HOST_TEST_DIR = 'C:\path\to\new-empty-owned-scratch'
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host --test windows_host -- --ignored --nocapture --test-threads=1

It checks the `Nimbo/<version>` UA header at the fixture, the native title following `document.title`, absence of `window.nimboDesktop`, native cancellation of `getDisplayMedia()` after a real input click, popup denial plus recorded external open, a cancelled cross-origin navigation recorded as external, second-instance restore/focus and quit on close. It prints REAL_WINDOWS_HOST_POLICY_OK only after all assertions pass. The side-by-side Electron comparison is the operator checklist in `desktop/tests/e2e/shell-parity.md`.

The manual `windows.yml` workflow (dispatch requires separate coordinator authorization) now builds this release shell into the `Nimbo-Rust-Windows-PREVIEW-not-release` artifact without executing it. The previous diagnostic workflow run 42827156882 validated only the older a5d88fc probe.

## Executable native capture experiment (interactive Windows ONLY)

Run the following from a clean checkout of the recorded capture-continuation commit, in an interactive Windows x64 desktop with hardware D3D11, WGC and WebView2. WGC HWND interop requires Windows 10 1903 or later; actual supported OS/runtime/device evidence is still required. These tests deliberately capture only their OWN native test window, never accept a script-supplied HWND, and do not open the system picker or imply full Nimbo picker parity.

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --test windows_capture -- --ignored --nocapture --test-threads=1

The test requires nonuniform actual window pixels, changes the source text, requires different captured pixels within five seconds and checks stop. It prints REAL_WGC_CHANGED_PIXELS only after those assertions pass.

For the complete local frame-to-track route, first create a NEW owned diagnostic scratch directory and set NIMBO_CAPTURE_TEST_DIR to its absolute path (PowerShell `$env:NIMBO_CAPTURE_TEST_DIR = 'C:\\path\\to\\owned-scratch'`), then:

    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --test windows_webview_capture -- --ignored --nocapture --test-threads=1

This opens separate source/receiver windows, starts an ephemeral 127.0.0.1-only fixed-page server and uses an isolated WebView profile beneath that scratch directory. It queries the actual native main-frame ID after navigation, denies subsequent navigation/subframe navigation/popups/permissions/system capture, and grants NO WebMessageReceived/native command handler. Test controls/ACKs are read back by host ExecuteScript in the fixed main document. The same production-candidate adapter is invoked with a test-local control callback, not fake media. It checks actual MediaStream and MediaStreamTrack instances, live state, at least three requestVideoFrameCallback notifications, then ended state after stop. REAL_WEBVIEW_MEDIASTREAM is printed only after live-render assertions. Stop/controller/window/server/apartment cleanup is scoped; retain the printed isolated profile for diagnostics and remove ONLY that profile after its WebView browser has exited. The test never logs credentials or contacts Firebase. Enforce an external test-process timeout if COM/runtime callbacks hang.

This local test is the next demonstrable boundary, NOT unchanged-community WebRTC publication or an independent receiver. It has not run in this Linux continuation. Normal Windows `cargo test` skips both ignored hardware tests; CI green with skips does not close checkpoint 2. The artifact-only workflow does not package these test executables. Coordinator must arrange these exact commands on an interactive source checkout; running a PREVIEW artifact cannot exercise them.

Diagnostic limitations are explicit: own-process window only; fixed size (resize/device/source loss closes instead of leaking stale pixels); SDR BGRA readback/opaque RGBA; two outstanding mappings; fresh immutable mapping per frame; 2-second ACK expiry; publication interval at least 17 ms; no audio/game/login implementation. The adapter accepts ideal constraints and max frameRate >=60 only, rejects narrower max/min/exact/audio, revokes ALL clones when any clone stops, and disposes on pagehide/missing frames. Full constraints/settings/ended semantics, 30 fps mode, performance/HDR/minimized/game privacy, picker parity, main-host IPC, login, independent WebRTC and resource-cycle evidence remain gates. No prototype behavior is installed into the hosted app or declared parity.

## Commit-bound evidence checklist

Before recording ANY Windows result, include `git rev-parse HEAD`, `git status --short`, `rustc +1.99.1 --version`, Windows edition/build, architecture, WebView2 runtime, builder tool/SDK versions, exact command, exit status and output. Record artifact SHA-256 and bytes; size is informational. This worker does not push/dispatch; coordinator owns those authorized actions. Release/deploy/merge remain forbidden. Workflow bytes change only through exact approved blob exceptions.

Checkpoint 1 host evidence (all pending):

- [ ] Windows link/tests/clippy of this commit (PREVIEW workflow or local builder).
- [ ] `windows_host` interactive test output with REAL_WINDOWS_HOST_POLICY_OK.
- [ ] Manual release check: hosted login screen visible, title "Nimbo 0.4.1", no login performed.
- [ ] Parity matrix `desktop/tests/e2e/shell-parity.md` filled from a side-by-side Electron 0.4.1 run.

Capture groundwork evidence checklist (retained from the previous plan; runtime pending):

- [ ] Windows link/tests/clippy for this capture-continuation commit (prior a5d88fc Windows link passed).
- [ ] Real host integration: create a blank WebView, install DenyGuards before navigation, missing-interface/registration/deny-failure rollback and controller closure. Current guard library code is only cross-typechecked.
- [ ] Execute both real Windows hardware tests above; record actual native pixels and WebView MediaStream/render/stop results. Implementation exists; runtime proof remains pending.
- [ ] Unchanged branch source in isolated emulator mode publishes that REAL track to an independent browser; remote hosted release configuration stays unchanged. No fakeMedia=1/native-evidence substitution.
- [ ] Choose two distinct windows/monitors and observe the chosen source, constraints at 720/1080 and 30/60 fps, timestamps/receiver dimensions/frame counts/latency and camera independence. No inferred success from a local preview.
- [ ] Cancel/reselect; stop/native-ended/close/reload/navigation; stale session/ACK; clone/lifetime cleanup and bounded resources after repeated cycles.
- [ ] Denied foreign/same-origin iframe, opaque/unknown frame, forged native command/source/generation and unsupported interface paths, observed in the actual native host.
- [ ] Source/audio-intent/game PID binding retained for later ports; no claim of implemented app audio or game focus privacy yet. Full app-only/no-Nimbo audio, Alt+Tab/game exit, picker parity, installer/updater and session persistence are still mandatory later gates.

Only real Windows evidence can close these gates. Portable passing tests, a source check, an installed runtime or a linked artifact cannot. Later checkpoints (bridge, auth/deep link, capture, audio, game watch, updater, installer) need their own approval.
