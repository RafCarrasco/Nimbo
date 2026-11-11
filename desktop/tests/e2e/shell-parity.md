# Checkpoint 1 — Windows shell parity matrix (operator checklist)

STATUS: NOT VERIFIED. This file is the checklist that task 1.4 delivers. It holds NO results. Fill in
the "observed" columns only from a real side-by-side run on one Windows machine and attach the
evidence listed below. A Linux compile, a Windows-target `cargo check`/`clippy`, a linked artifact or
a green CI run is not a parity observation.

Rows marked "decide" are behaviors that Electron's source does not express. Any difference between
the two columns is a finding for the user to approve (plan Open Question E). It is never silently
resolved in either direction, and the Rust code is not changed to "match" without that approval.

## Safety rules for the operator

- Use the hosted site only up to the login screen. Do not sign in, and do not enter production
  credentials, in either app. A login check belongs to a later checkpoint.
- Electron oracle: the installed Nimbo 0.4.1 release. Record the SHA-256 of `Nimbo-Setup.exe` before
  installing it.
- Rust: the RELEASE build (`cargo +1.99.1 build --manifest-path desktop/rust/Cargo.toml --locked --release`
  or the `Nimbo-Rust-Windows-PREVIEW-not-release` artifact). Record its SHA-256 and the commit. Never
  run the `test-host` build against the hosted site; it refuses to start that way.
- Do not replace or uninstall the Electron installation. The Rust preview keeps its WebView2 profile in
  `%LOCALAPPDATA%\Nimbo\WebView2`.

## Evidence header (required before any row is filled)

| Field | Value |
|---|---|
| `git rev-parse HEAD` / `git status --short` | |
| Windows edition, version and build, architecture | |
| Display scaling(s) and monitor layout | |
| WebView2 Runtime version (`edge://version` is not available; use Apps & Features or the registry) | |
| Electron installer SHA-256 / installed version | |
| Rust executable SHA-256 / size / build command or CI run id | |
| Operator, date | |

## Shell UI brief (identity retained from Electron, no redesign)

- Job: a single desktop window that shows the published Nimbo site, the same as the Electron app.
- Window: 1280×800 DIP centered, minimum 850×590 DIP, title "Nimbo" until the page sets
  "Nimbo <version>", icon from `desktop/icon.ico`, background `#0b1020`, no menu bar
  (`desktop/main.cjs:695-711`).
- Content: the hosted site itself. The shell adds no UI of its own except a native error dialog
  when WebView2 cannot start ("O Nimbo não conseguiu abrir…"); Electron has no equivalent dialog,
  so this is listed below as a decision.
- Anti-goals: no web redesign, no new colors/tokens, no shell chrome, no bridge or stub features.

## Matrix

"Rust CP1 intent" states what the code is written to do. It is not an observation.

| # | Behavior | How to test | Electron 0.4.1 observed | Rust CP1 intent | Rust observed | Same? / finding |
|---|---|---|---|---|---|---|
| 1 | F12 | press on the login screen | | WebView2 DevTools disabled (`AreDevToolsEnabled=false`) | | |
| 2 | Ctrl+Shift+I | same | | disabled (same setting) | | |
| 3 | Ctrl+mouse wheel zoom | over the page | | WebView2 default (zoom enabled), decide | | |
| 4 | Ctrl+= / Ctrl+- / Ctrl+0 | same | | WebView2 default, decide | | |
| 5 | F11 | same | | WebView2 default, decide | | |
| 6 | Ctrl+P | same | | WebView2 default (print UI), decide | | |
| 7 | Ctrl+F | same | | WebView2 default (find bar), decide | | |
| 8 | Alt+F4 | with the window focused | | closes the window and quits (no other window) | | |
| 9 | Context menu | right click on page, on a text field, on a link | | none (`AreDefaultContextMenusEnabled=false`) | | |
| 10 | External link | open a non-app https link (e.g. a footer/help link) | | navigation cancelled, opened in the default browser | | |
| 11 | Popup | `target=_blank` / `window.open` to https | | never an app window; https opens in the default browser | | |
| 12 | F5 | on the login screen | | reload once (handled; no double reload) | | |
| 13 | Ctrl+R and Ctrl+Shift+R | same | | reload once | | |
| 14 | Title | read the taskbar/window title | | "Nimbo 0.4.1" after load (UA `Nimbo/0.4.1`) | | |
| 15 | Minimum size | drag the corner to the smallest size | | 850×590 DIP, scaled per monitor DPI | | |
| 16 | Second instance | launch again while running; also while minimized | | first window restored and focused, second exits | | |
| 17 | Renderer crash | Task Manager → end the renderer process of the app | | fixed log code, window kept (no auto-reload), decide | | |
| 18 | Link hover status bubble | hover a link | | hidden (`IsStatusBarEnabled=false`), decide | | |
| 19 | Autofill / password save prompts | focus the email field | | disabled, decide | | |
| 20 | HTML element fullscreen (LiveTile) | needs a signed-in live view: NOT testable at CP1 without login | | DEFERRED: fills the WebView only, no window fullscreen | | |
| 21 | Downloads | (if any download link exists before login) | | WebView2 default download UI, decide | | |
| 22 | Empty document title | n/a on the hosted site; observe only if seen | | falls back to "Nimbo" | | |
| 23 | Taskbar grouping / icon | pin, then launch | | AppUserModelID `app.nimbo.desktop`, embedded icon | | |
| 24 | Window placement on a small screen / at 150% | change scaling, restart | | centered; origin kept inside the work area | | |
| 25 | Moving between monitors with different DPI | drag across | | resized to the suggested rect (`WM_DPICHANGED`) | | |
| 26 | WebView2 Runtime missing | only on a disposable VM without the runtime | n/a (Electron bundles Chromium) | native error dialog, exit 1, decide | | |
| 27 | Background throttling (window covered/minimized) | covered by later live checkpoints | | same 4 Chromium switches as Electron; WebView never hidden on minimize | | |

## Automated interactive host test (test-host build, loopback fixture only)

Run on an interactive Windows x64 desktop from a clean checkout of the recorded commit. It never opens
the hosted site, never logs in and never calls ShellExecute (external opens are recorded under the
scratch directory):

    $env:NIMBO_HOST_TEST_DIR = 'C:\path\to\new-empty-owned-scratch'
    cargo +1.99.1 test --manifest-path desktop/rust/Cargo.toml --locked --features test-host --test windows_host -- --ignored --nocapture --test-threads=1

Pass requires the line `REAL_WINDOWS_HOST_POLICY_OK`. Record the full output, exit status and the
`events.log` beneath the printed `ISOLATED_HOST_DIR`. Delete that directory only after the test-host
and its WebView2 processes have exited. Enforce an external timeout if the run hangs.

## Screenshot checklist (operator evidence, not stored in the repository)

Capture each with both apps where applicable; store outside the repository and record file name,
SHA-256 and the row number here:

- [ ] Startup, login screen visible, 100% scaling (Electron, Rust).
- [ ] Same at 150% scaling.
- [ ] Minimum window size (row 15).
- [ ] Context menu attempt (row 9) and any other row that differs.
- [ ] Taskbar with title and icon (rows 14, 23).

No screenshot or GUI claim may be made from this checklist until the files exist and were inspected.
