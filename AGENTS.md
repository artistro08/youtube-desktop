# AGENTS.md

## What this is

A YouTube desktop app for Windows, forked from [Pake](https://github.com/tw93/Pake)
and cut down to that one job. Pake's CLI, its docs, its CI and its other app
presets have been removed; what remains is the Tauri application.

- **Stack**: Tauri v2 (Rust) + WebView2. No frontend framework, no build step
  for the page — the window is pointed at `youtube.com` and everything this
  project adds is injected into it.
- **Platform**: Windows only. The macOS and Linux bundle configs are gone,
  though platform-gated Rust from upstream is still compiled out by `cfg`.

## Building

```bash
npm install
npm run build        # MSI in src-tauri/target/release/bundle/msi
npm run dev          # run without packaging
cd src-tauri && cargo test
```

`src-tauri/frontend/` is a placeholder that exists only because Tauri wants a
frontend directory to bundle. Nothing in it is ever shown.

## Where things live

| Path | What lives there |
| --- | --- |
| `src-tauri/src/lib.rs` | Setup, command registration, window events, the hide/reveal playback path |
| `src-tauri/src/app/window.rs` | Window construction, WebView2 flags, `eval_with_user_gesture` |
| `src-tauri/src/app/setup.rs` | Tray icon and menu |
| `src-tauri/src/app/settings.rs` | The settings record, its file, and its defaults |
| `src-tauri/src/app/youtube.rs` | Link routing, deep links, notifications |
| `src-tauri/src/app/media.rs` | Windows media transport controls |
| `src-tauri/src/app/pip_window.rs` | Puts the app's icon on the floating picture-in-picture window |
| `src-tauri/src/inject/titlebar.js` | Integrated title bar, picture-in-picture, playback on hide |
| `src-tauri/src/inject/youtube.js` | Notification polling, settings dialog, copy shortcut |
| `src-tauri/pake.json` | Start URL, window size, `internal_url_regex` |

Files under `src-tauri/src/inject/` other than `titlebar.js` and `youtube.js`
are upstream Pake behaviour (context menu, find bar, toasts, styles) and are
still wired up in `window.rs`.

## Things that will catch you out

**Trusted Types.** YouTube sends `require-trusted-types-for 'script'`, so
assigning `innerHTML` throws — and it takes the rest of the script with it, so
a whole feature disappears with no visible error. Build DOM with
`createElement`, `createElementNS` and `textContent`.

**The page cannot see the window.** `CalculateNativeWinOcclusion` is disabled
so playback survives being hidden, which means a minimized window still reports
itself visible and `visibilitychange` never fires. Window state is watched in
Rust (`WindowEvent::Resized` plus `is_minimized`) and pushed to the page.

**Picture-in-picture needs a user gesture.** A plain `eval` is refused with
`NotAllowedError`. Anything reaching `requestPictureInPicture` from outside a
click has to go through `eval_with_user_gesture`, which uses WebView2's
DevTools protocol with `userGesture: true`.

**`position: fixed` on `ytd-app` breaks YouTube's dialogs.** It makes a
stacking context, which traps dialogs under the backdrop Polymer appends to
`body`; they render, dimmed, and swallow every click. It is `absolute` for that
reason — see the comment in `titlebar.js`.

**Settings live in two halves.** A field means: the struct and its `serde`
default in `settings.rs`, the accessor, and a row in the dialog in
`youtube.js`. One command (`set_app_setting`) covers every switch, so no new
IPC is needed. They are stored under the bundle identifier
(`%APPDATA%\com.artistro08.youtube\settings.json`), so changing the identifier
moves the file — `settings.rs` reads the previous directory once for that
reason.

**Scripts are injected into every frame.** YouTube's live chat is an iframe on
the watch page, so anything that builds UI or reports state guards with
`window.top !== window.self`, or the chat panel grows its own title bar and
reports itself as the page to resume on. The outbound-link handler is the
deliberate exception: chat messages contain links too.

## Rust rules

- No `panic!` / `unwrap()` / `expect()` on user-reachable paths: IPC commands,
  config loading, event handlers. Use `?`, or log and carry on where the
  failure is cosmetic.
- A silent `let _ = ...` on a fallible native call needs a comment saying why
  the failure is acceptable, or an `eprintln!` surfacing it.
- The page is untrusted. Every `#[tauri::command]` validates its input at the
  boundary: URLs checked with `is_youtube_url`, text bounded with
  `truncate_page_text`, setting names matched against a known set.
- `cargo fmt` and `cargo clippy` are clean; keep them that way.

## Keeping up with upstream

`upstream` points at tw93/Pake and its push URL is deliberately invalid, so a
stray push cannot reach it. Fetch and merge selectively — most of what upstream
changes now lives in files this fork has deleted.

## Licence

GPL-3.0-or-later, inherited from Pake. The output exception covers apps built
by Pake's standard process, not forks of Pake's own source, so this repository
is bound by the GPL. Keep `LICENSE` and `LICENSE-EXCEPTION` in place.


