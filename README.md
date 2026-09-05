# YouTube for Windows

A YouTube desktop app for Windows: YouTube in its own window, with a title bar
that is part of the page, picture-in-picture wired to the window buttons, and
the Windows media controls, notifications and links an installed app is
expected to have.

Built on [Pake](https://github.com/tw93/Pake), which packages a website into a
Tauri window. This fork replaces the generic wrapper with YouTube-specific
behaviour.

## Install

Download the `.msi` from [Releases](../../releases) and run it. Windows 11
already ships the WebView2 runtime the app renders with.

## What it does

### Window

The title bar **is** YouTube's masthead. The minimize, maximize and close
buttons sit at the right end of that same row, drawn the way Windows draws
them, and the empty space between the search box and the avatar drags the
window.

On a watch page the bar starts clear, so the player's ambient glow reaches the
top of the window, and fades to solid over 300ms once the page scrolls.

Scrollbars are Chromium's overlay style: a thin line when idle that widens
under the pointer, with the page running to the window edge.

### Picture-in-picture

The video pops out into a floating window when you minimize, when you close to
the tray, from the tray menu, and from the video's own right-click menu.
Bringing the window back takes the video out of the floating window again.

Every route to a hidden window behaves the same — the title bar's buttons, the
taskbar, <kbd>Alt</kbd>+<kbd>F4</kbd>, and the tray — because the app watches
the window rather than its own buttons.

Shorts stay put unless you ask for them, and a playing Short is paused when the
window is hidden. A full video keeps playing, which is the point of an app that
lives in the tray.

### Windows integration

- **Tray icon** with Show/Hide, picture-in-picture for the current video, and a
  switch to remove the icon. The in-app settings dialog is the way back.
- **Media controls.** The volume flyout and the keyboard's media keys show the
  app by name, with the video's title and thumbnail.
- **Notifications.** YouTube's own notifications arrive as native toasts;
  clicking one opens that video in the app.
- **Deep links.** `youtube://` opens in the app. Links that leave YouTube open
  in your default browser.
- <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies the current video's
  canonical URL, with the snackbar YouTube shows when it saves something.

### Settings

Under the avatar menu, below YouTube's own Settings:

| Setting | Default | What it does |
| --- | --- | --- |
| Show tray icon | On | Closing the window keeps the app running in the tray. |
| Continue where you left off | On | Reopens the last page instead of the home feed. |
| Pause Shorts when hidden | On | Minimizing or closing pauses a playing Short. |
| Picture-in-picture on minimize | On | Minimizing pops a playing video out. |
| Picture-in-picture on close | On | Closing to the tray does the same. |
| Include Shorts | Off | Lets Shorts pop out too. |

Stored in `%APPDATA%\com.artistro08.youtube\settings.json`. Every field has a
default, so a file written by an older build keeps working.

## Memory

The window is one site, so most of what Chromium spends memory on to keep a
browser fast across many tabs is waste here. The app asks for a single
renderer, folds same-site frames together, turns off the back/forward cache,
and runs Chromium's low-end device memory profile.

Measured on the same watch page, whole process tree:

| | Processes | Private | Working set |
| --- | --- | --- | --- |
| v0.1.0 | 9 | 658 MB | 1032 MB |
| v0.1.1 | 8 | 608 MB | 937 MB |

Private bytes is the honest figure; working set double counts pages shared
between the WebView2 processes. What remains is a renderer and a GPU process
holding a decoded video, which is the floor for playing YouTube at all.

Site isolation is deliberately left on. Turning it off would collapse more
processes, but it is the boundary between the page and the ad frames it
embeds, and that is not a memory decision.

## Building

Needs [Rust](https://rustup.rs), Node 20+, and the MSVC build tools.

```bash
npm install
npm run build     # MSI in src-tauri/target/release/bundle/msi
npm run dev       # run without packaging
```

The Rust tests are pure and need no webview:

```bash
cd src-tauri && cargo test
```

## How it is put together

There is no frontend. The window is pointed at `youtube.com` and everything
this project adds is either Rust, or JavaScript injected into that page.

| Path | What lives there |
| --- | --- |
| `src-tauri/src/lib.rs` | App setup, window events, the minimize/close playback path |
| `src-tauri/src/app/window.rs` | Window construction, WebView2 flags, page evaluation |
| `src-tauri/src/app/setup.rs` | Tray icon and its menu |
| `src-tauri/src/app/settings.rs` | The settings record and its file |
| `src-tauri/src/app/youtube.rs` | Link routing, deep links, notifications |
| `src-tauri/src/app/media.rs` | Windows media transport controls |
| `src-tauri/src/app/pip_window.rs` | Gives the floating window the app's icon |
| `src-tauri/src/inject/titlebar.js` | The integrated title bar and playback control |
| `src-tauri/src/inject/youtube.js` | Notification polling, settings dialog, copy shortcut |
| `src-tauri/pake.json` | Window size, start URL, which hosts stay in the app |

Two notes for anyone changing the injected scripts:

- YouTube sends `require-trusted-types-for 'script'`, so assigning `innerHTML`
  throws and takes the rest of the script with it. Build DOM with
  `createElement` and `textContent`.
- The page never learns that the window was minimized. Occlusion tracking is
  deliberately off so playback survives being hidden, which means
  `visibilitychange` never fires; the Rust side watches window events instead.

## Licence

GPL-3.0-or-later, inherited from Pake, whose source this modifies. Pake's
output exception covers apps produced by its standard build process, not forks
of Pake itself, so this repository and anything built from it stay under the
GPL. See [`LICENSE`](LICENSE) and [`LICENSE-EXCEPTION`](LICENSE-EXCEPTION).

Pake is copyright Tw93 and the Pake contributors. This project is not
affiliated with YouTube or Google.


