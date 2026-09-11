# YouTube for Windows

A YouTube desktop app for Windows: YouTube in its own window, with a title bar
that is part of the page, picture-in-picture wired to the window buttons, and
the Windows media controls, notifications and links an installed app is
expected to have. In my effort to help me separate one of the main apps I use, I decided to create this. It solves a problem for me, and if anyone else wants to use it, I hope it solves the problem for you.

Built on [Pake](https://github.com/tw93/Pake), which packages a website into a
Tauri window. This fork replaces the generic wrapper with YouTube-specific
behaviour.

## Install

Download the `.msi` from [Releases](../../releases) and run it. Windows 11
already ships the WebView2 runtime the app renders with.

## What it does / features

### Integrated Window Title Bar

The search bar, navigation, logo, etc., is all part of the title bar. You can grab and move any part of it when you want to drag the window.

### Auto Picture-in-picture

When you minimize the app, a picture‑in‑picture window will pop up. This will also happen when you close it to the tray. Shorts don’t do this by default, since it changes the height of the picture‑in‑picture, but you can enable it in Settings.

### Other features

- **Tray icon** with Show/Hide, picture-in-picture for the current video, and a
  switch to remove the icon. You can re‑enable the tray icon by going into the App Settings under your avatar.
- **Deep links.** `youtube://` opens in the app. You can add redirects to your browser to open these links in the app.
- <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies the current page's URL

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

## Memory

Since we're using a native WebView, we can leverage the built-in memory optimizations and keep the app's memory usage as low as possible.

## Building

Needs [Rust](https://rustup.rs), Node 20+, and the MSVC build tools.


## Contributing
If you would like to contribute and add any new features, feel free to do so.

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


