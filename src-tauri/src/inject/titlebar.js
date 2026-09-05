// Integrated title bar.
//
// The window is frameless, and YouTube's masthead doubles as the caption: the
// minimize / maximize / close buttons sit at the right end of that same row,
// full row height, and the empty space around YouTube's own controls drags the
// window. Everything is built with createElement because YouTube serves
// `require-trusted-types-for 'script'`, under which innerHTML throws.
//
// This runs on every page in the webview, not only YouTube. A sign-in page on
// accounts.google.com has no masthead, and without controls of its own the
// window would have no way to be moved or closed.

(function () {
  const tauri = window.__TAURI__;
  if (!tauri?.window) return;
  // Scripts are injected into every frame, and YouTube's live chat is an
  // iframe: without this, the chat panel grows its own set of window controls.
  if (window.top !== window.self) return;
  if (window.__pakeTitleBar) return;
  window.__pakeTitleBar = true;

  const appWindow = tauri.window.getCurrentWindow();
  const invoke = tauri.core?.invoke;
  if (!invoke) return;

  const CONTROLS_ID = "pake-window-controls";
  const STYLE_ID = "pake-window-controls-style";
  // Marks a page that has YouTube's own layout, and so can have its scrolling
  // moved below the title bar. Other pages keep scrolling normally.
  const YOUTUBE_CLASS = "pake-youtube";
  // Marks a watch page, where the bar starts transparent.
  const WATCH_CLASS = "pake-watch";
  // Marks a scroller that has moved off the top, which is what turns the watch
  // page's bar solid again.
  const SCROLLED_CLASS = "pake-scrolled";
  // Set once the player has been measured, so the lifted ambient glow is never
  // pinned to a box of zeroes before there are real numbers for it.
  const GLOW_CLASS = "pake-glow";
  // Windows' own caption buttons are 46px wide.
  const BUTTON_WIDTH = 46;
  const CONTROLS_WIDTH = BUTTON_WIDTH * 3;
  // Height to fall back on where there is no masthead to match.
  const FALLBACK_HEIGHT = 32;
  // How long the watch page's bar takes to fade between clear and solid.
  const MASTHEAD_FADE_MS = 300;
  // How still the window has to be before a drag counts as finished. Long
  // enough to cover the gap between two frames of a slow drag, short enough
  // that letting go feels like it settles at once.
  const SETTLE_MS = 120;
  // Half-second tries at finding YouTube's layout before concluding this page
  // does not have one. Thirty seconds is far longer than a cold load takes.
  const LAYOUT_ATTEMPTS = 60;

  // Segoe Fluent Icons / Segoe MDL2 Assets caption glyphs — the same
  // characters Windows draws in its own title bars.
  const GLYPH_MINIMIZE = "\uE921";
  const GLYPH_MAXIMIZE = "\uE922";
  const GLYPH_RESTORE = "\uE923";
  const GLYPH_CLOSE = "\uE8BB";

  // Anything in this list keeps its click instead of starting a window drag.
  const INTERACTIVE_SELECTOR = [
    "a",
    "button",
    "input",
    "textarea",
    "select",
    "iframe",
    "[role='button']",
    "[role='menuitem']",
    "[role='combobox']",
    "[contenteditable='true']",
    "[tabindex]:not([tabindex='-1'])",
    "yt-icon-button",
    "ytd-button-renderer",
    "ytd-searchbox",
    "tp-yt-paper-button",
    "tp-yt-paper-icon-button",
    `#${CONTROLS_ID}`,
  ].join(",");

  function masthead() {
    return document.querySelector("ytd-masthead");
  }

  function titleBarHeight() {
    const height = masthead()?.offsetHeight || 0;
    return height > 0 ? height : FALLBACK_HEIGHT;
  }

  // Styles
  function installStyles() {
    if (document.getElementById(STYLE_ID)) return;

    const style = document.createElement("style");
    style.id = STYLE_ID;
    // textContent is not a Trusted Types sink, unlike innerHTML.
    style.textContent = `
      /* Pake's own 20px drag strip would sit on top of the masthead and eat
         clicks meant for the search box and logo. This row handles dragging. */
      #pake-top-dom {
        display: none !important;
      }

      #${CONTROLS_ID} {
        position: fixed;
        top: 0;
        right: 0;
        display: flex;
        height: var(--pake-titlebar-height, ${FALLBACK_HEIGHT}px);
        /* Above the masthead (2020) but below YouTube's dialogs. */
        z-index: 2500;
        -webkit-user-select: none;
        user-select: none;
      }

      #${CONTROLS_ID} button {
        width: ${BUTTON_WIDTH}px;
        height: 100%;
        margin: 0;
        padding: 0;
        border: 0;
        background: transparent;
        color: #0f0f0f;
        font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets", sans-serif;
        font-size: 10px;
        line-height: 1;
        cursor: default;
        transition: background-color 0.1s ease;
      }

      #${CONTROLS_ID} button:hover {
        background-color: rgba(0, 0, 0, 0.05);
      }

      #${CONTROLS_ID} button:active {
        background-color: rgba(0, 0, 0, 0.09);
      }

      /* Windows paints the close button red on hover, glyph inverted. */
      #${CONTROLS_ID} button.close:hover {
        background-color: #c42b1c;
        color: #ffffff;
      }

      #${CONTROLS_ID} button.close:active {
        background-color: #b8271b;
        color: #ffffff;
      }

      html[dark] #${CONTROLS_ID} button {
        color: #f1f1f1;
      }

      html[dark] #${CONTROLS_ID} button:hover {
        background-color: rgba(255, 255, 255, 0.08);
      }

      html[dark] #${CONTROLS_ID} button:active {
        background-color: rgba(255, 255, 255, 0.12);
      }

      /* Keep YouTube's own end-of-row controls clear of the caption buttons.
         The avatar carries its own padding, so no gap is added on top. */
      ytd-masthead #end.ytd-masthead {
        margin-right: ${CONTROLS_WIDTH}px;
      }

      /* A video playing full screen owns the whole surface. */
      html:has(:fullscreen) #${CONTROLS_ID} {
        display: none;
      }

      /* On a watch page the bar starts clear so the player's ambient glow
         reaches the top of the window, then fades to its normal solid colour as
         soon as the page leaves the top. YouTube's own background element is
         faded rather than a colour of ours, so it keeps whatever material
         YouTube uses. */
      html.${WATCH_CLASS} ytd-masthead #background {
        opacity: 0 !important;
        transition: opacity ${MASTHEAD_FADE_MS}ms ease !important;
      }

      html.${WATCH_CLASS}.${SCROLLED_CLASS} ytd-masthead #background {
        opacity: 1 !important;
      }

      /* The ambient glow, lifted out of the scroller.
         The glow is drawn around the player and bleeds upward past it, which is
         what should show through the clear bar. The scroller starts below the
         bar and clips its own contents, so that bleed is exactly the part that
         gets cut off. Switching the glow to fixed positioning takes it out of
         the scroller's clip while leaving it where it was on screen and where
         it was in the paint order, so it still sits behind the player and
         behind the masthead. Only while the page is at the top: the geometry
         below is measured for that position, and once scrolled the bar is
         solid and there is nothing to show through anyway. */
      html.${WATCH_CLASS}.${GLOW_CLASS}:not(.${SCROLLED_CLASS}) #cinematics {
        position: fixed !important;
        left: var(--pake-glow-left, 0px) !important;
        top: var(--pake-glow-top, 0px) !important;
        width: var(--pake-glow-width, 0px) !important;
        height: var(--pake-glow-height, 0px) !important;
      }

      /* Scrolling below the title bar.
         A classic Windows scrollbar is painted outside the page's viewport, so
         page content can never be drawn over it: with the document scrolling,
         the scrollbar runs the full window height and the caption buttons are
         stuck to the left of it. Moving the scroll into ytd-app, which starts
         below the title bar, gives the row the full window width and starts the
         scrollbar underneath it — still a native scrollbar, just on a different
         element. Only applied once YouTube's own layout is present. */
      html.${YOUTUBE_CLASS},
      html.${YOUTUBE_CLASS} body {
        height: 100%;
        overflow: hidden;
      }

      html.${YOUTUBE_CLASS} ytd-app {
        /* Absolute, not fixed, and the difference matters. A fixed element is
           a stacking context, which would trap YouTube's dialogs inside this
           one: Polymer opens a dialog in ytd-app but appends its backdrop to
           the body, so the backdrop's z-index would be compared against this
           element rather than against the dialog's, and the backdrop would end
           up over the dialog. The dialog still renders, dimmed, but every
           click lands on the backdrop instead — the share and feedback dialogs
           went dead that way. With html and body pinned to the window and not
           scrolling, absolute resolves against the same box fixed did. */
        position: absolute;
        top: var(--pake-titlebar-height, ${FALLBACK_HEIGHT}px);
        right: 0;
        bottom: 0;
        left: 0;
        /* YouTube sizes ytd-app to the full viewport and puts a floor under it
           with min-height, which the bottom offset alone cannot beat: the
           container would hang one title-bar height past the window edge,
           cutting off the end of the page and the scrollbar's bottom arrow. */
        height: calc(100% - var(--pake-titlebar-height, ${FALLBACK_HEIGHT}px)) !important;
        min-height: 0 !important;
        overflow-x: hidden;
        overflow-y: auto;
      }

      /* YouTube offsets the page by the masthead height to clear its own fixed
         masthead. The scroller already starts below it, so that would count the
         same 56px twice. */
      html.${YOUTUBE_CLASS} ytd-page-manager {
        margin-top: 0 !important;
      }

    `;

    document.head.appendChild(style);
  }

  // Buttons
  function button(className, glyph, label, onClick) {
    const element = document.createElement("button");
    element.className = className;
    element.textContent = glyph;
    element.setAttribute("aria-label", label);
    element.setAttribute("title", label);
    element.addEventListener("click", onClick);
    return element;
  }

  let maximizeButton = null;

  /// The page is on a video page, and has a video that can be popped out.
  ///
  /// The path is part of it because the home feed keeps a video element around
  /// for its hover previews, and a menu item offering to pop that out would
  /// float whatever thumbnail the pointer last passed over.
  function onVideoPage() {
    const path = window.location.pathname;
    return path === "/watch" || path.startsWith("/shorts");
  }

  /// The video to pop out, if there is one.
  ///
  /// Not the first in the document: a Shorts feed and the watch page's inline
  /// previews both keep several video elements around.
  function pictureInPictureVideo(requirePlaying) {
    return [...document.querySelectorAll("video")].find(
      (candidate) =>
        candidate.readyState > 0 &&
        !candidate.disablePictureInPicture &&
        (!requirePlaying || (!candidate.paused && !candidate.ended)),
    );
  }

  /// Put the current video into picture-in-picture.
  ///
  /// Hiding the window pops out only a video that is actually playing, since a
  /// paused page has nothing worth floating above the desktop, and leaves
  /// Shorts alone unless `shortsMayPopOut` says otherwise — scrolling the
  /// Shorts feed would otherwise throw a clip into a floating window on every
  /// minimize. Asking for it outright, from the tray or the context menu,
  /// means neither restriction applies.
  ///
  /// Must be called straight from a click, or through the app's
  /// gesture-carrying eval: Chromium rejects the request without user
  /// activation unless something is already in picture-in-picture.
  function enterPictureInPicture(options) {
    if (!document.pictureInPictureEnabled) return Promise.resolve();

    const explicit = options?.explicit === true;
    const onShorts = window.location.pathname.startsWith("/shorts");
    if (onShorts && !explicit && !options?.shortsMayPopOut) {
      return Promise.resolve();
    }

    const video = pictureInPictureVideo(!explicit);

    if (!video || document.pictureInPictureElement === video) {
      return Promise.resolve();
    }

    // "Back to tab" in the floating window's own controls hands the video back
    // to the page and nothing else, so the app is still minimized or in the
    // tray with the video playing where it cannot be seen. Bring the window
    // back up with it. Chromium reports the floating window being closed
    // outright the same way, so that raises the window too.
    video.addEventListener("leavepictureinpicture", returnToWindow, {
      once: true,
    });

    return video.requestPictureInPicture().catch((error) => {
      video.removeEventListener("leavepictureinpicture", returnToWindow);
      console.warn("[Pake] Picture-in-picture was refused:", error);
    });
  }

  // The tray's picture-in-picture item comes in through here.
  window.__pakeEnterPictureInPicture = () =>
    enterPictureInPicture({ explicit: true });

  /// Raise the app window from wherever picture-in-picture left it.
  ///
  /// It may have been minimized or hidden to the tray, and either has to be
  /// undone before focusing, so all three run.
  function returnToWindow() {
    appWindow.show().catch(() => {});
    appWindow.unminimize().catch(() => {});
    appWindow.setFocus().catch(() => {});
  }

  /// Settle the page's playback because the window is going out of sight.
  ///
  /// Picture-in-picture first, since a video handed to a floating window is
  /// still being watched and must keep playing. Only what stays behind in the
  /// hidden window is paused, and only a Short: a full video left running is
  /// the point of a tray app, while a Short loops out of sight until the feed
  /// moves on by itself.
  ///
  /// Called from the app, which watches the window rather than the buttons, so
  /// this runs for the taskbar and Alt+F4 as well as the title bar's own
  /// controls. The app supplies the picture-in-picture setting that applies to
  /// the way the window was dismissed.
  window.__pakeHidePlayback = hidePlayback;

  /// Take a floating video back into the page.
  ///
  /// The counterpart to the above, for when the window comes back into view.
  /// A picture-in-picture window that outlives the window it was popped out of
  /// leaves the video playing in two places at once as far as the user is
  /// concerned: one they are looking at, one floating over it.
  window.__pakeRestorePlayback = restorePlayback;

  function restorePlayback() {
    if (!document.pictureInPictureElement) return Promise.resolve();

    return document.exitPictureInPicture().catch((error) => {
      console.warn("[Pake] Could not return the video to the window:", error);
    });
  }

  function hidePlayback(options) {
    const pip = options?.pictureInPicture
      ? enterPictureInPicture({ shortsMayPopOut: options.shortsMayPopOut })
      : Promise.resolve();

    return pip.finally(() => {
      if (!options?.pauseShorts) return;
      if (!window.location.pathname.startsWith("/shorts")) return;

      const short = pictureInPictureVideo(true);
      if (!short || document.pictureInPictureElement === short) return;

      short.pause();
    });
  }

  function syncMaximizeGlyph() {
    if (!maximizeButton) return;

    appWindow
      .isMaximized()
      .then((maximized) => {
        maximizeButton.textContent = maximized ? GLYPH_RESTORE : GLYPH_MAXIMIZE;
        const label = maximized ? "Restore" : "Maximize";
        maximizeButton.setAttribute("aria-label", label);
        maximizeButton.setAttribute("title", label);
      })
      .catch(() => {});
  }

  function installControls() {
    if (document.getElementById(CONTROLS_ID)) return;

    const controls = document.createElement("div");
    controls.id = CONTROLS_ID;

    maximizeButton = button("maximize", GLYPH_MAXIMIZE, "Maximize", () => {
      appWindow
        .toggleMaximize()
        .then(syncMaximizeGlyph)
        .catch(() => {});
    });

    // Neither button settles playback itself. The app watches the window for
    // that, so minimizing from the taskbar or closing with Alt+F4 behaves the
    // same as clicking here.
    controls.appendChild(
      button("minimize", GLYPH_MINIMIZE, "Minimize", () => {
        appWindow.minimize().catch(() => {});
      }),
    );
    controls.appendChild(maximizeButton);
    controls.appendChild(
      // Honours the close-to-tray setting: this goes through the app's own
      // close handler rather than terminating the process.
      button("close", GLYPH_CLOSE, "Close", () => {
        appWindow.close().catch(() => {});
      }),
    );

    document.body.appendChild(controls);
    syncMaximizeGlyph();
  }

  // Dragging
  function isInteractive(event) {
    // composedPath crosses shadow roots, which closest() cannot: most of
    // YouTube's masthead controls live inside custom elements.
    return event
      .composedPath()
      .some(
        (node) =>
          typeof node?.matches === "function" &&
          node.matches(INTERACTIVE_SELECTOR),
      );
  }

  function withinTitleBar(event) {
    // A video playing full screen owns the whole surface. The masthead is still
    // laid out behind it, so without this the top of the video would start a
    // window drag and swallow click-to-pause and double-click-to-exit.
    if (document.fullscreenElement) return false;

    return event.clientY < titleBarHeight();
  }

  function onMouseDown(event) {
    if (event.button !== 0 || !withinTitleBar(event)) return;
    if (isInteractive(event)) return;

    appWindow.startDragging().catch(() => {});
  }

  function onDoubleClick(event) {
    if (!withinTitleBar(event) || isInteractive(event)) return;

    appWindow
      .toggleMaximize()
      .then(syncMaximizeGlyph)
      .catch(() => {});
  }

  // Layout
  //
  // Dragging a window edge fires `resize` many times a second, and the observers
  // below fire alongside it. Doing the work per event meant reading layout and
  // writing custom properties over and over within a single frame, and every
  // read forced a fresh layout that the write before it had just invalidated.
  // Everything is funnelled through one animation frame instead, which is the
  // rate the window is repainted at anyway.
  let syncScheduled = false;
  // True from the first resize event until the window has been still for
  // SETTLE_MS. Used to leave the expensive parts alone mid-drag.
  let resizing = false;
  let settleTimer = 0;

  function scheduleSync() {
    if (syncScheduled) return;

    syncScheduled = true;
    window.requestAnimationFrame(() => {
      syncScheduled = false;
      syncMetrics();
    });
  }

  /// Note that the window is being dragged, and arrange to notice when it stops.
  function beginResize() {
    resizing = true;
    window.clearTimeout(settleTimer);
    settleTimer = window.setTimeout(endResize, SETTLE_MS);
  }

  /// The window has stopped moving: put back everything the drag skipped.
  function endResize() {
    resizing = false;
    syncMaximizeGlyph();
    scheduleSync();
  }

  // Last values written, so an unchanged number never dirties the document.
  // `--pake-titlebar-height` positions and sizes ytd-app, so writing it is a
  // relayout of the whole page — and during a drag it is the same number every
  // single frame.
  const written = new Map();

  function setMetric(name, value) {
    if (written.get(name) === value) return;

    written.set(name, value);
    document.documentElement.style.setProperty(name, value);
  }

  function syncMetrics() {
    setMetric("--pake-titlebar-height", `${titleBarHeight()}px`);

    syncGlow();
  }

  function syncMastheadFade() {
    const scroller = document.querySelector("ytd-app");
    document.documentElement.classList.toggle(
      SCROLLED_CLASS,
      (scroller?.scrollTop ?? 0) > 0,
    );
  }

  /// Pin the lifted ambient glow over the player it belongs to.
  ///
  /// Measured from the player rather than from the glow itself, which is the
  /// element being moved: reading its own box back once it is fixed would just
  /// return the numbers written here and freeze it at the first size it had.
  // The player the glow is currently pinned to, and the observer watching it
  // for theatre mode and window resizes.
  let observedPlayer = null;
  let playerObserver = null;

  function syncGlow() {
    const root = document.documentElement;

    // The rule this feeds only matches on a watch page, so anywhere else the
    // measurement below would be taken and thrown away. Worth checking first:
    // this runs once a frame for the whole of a resize.
    if (!root.classList.contains(WATCH_CLASS)) {
      root.classList.remove(GLOW_CLASS);
      return;
    }

    const player =
      document.querySelector("#movie_player") ||
      document.querySelector("#player-container");

    if (player && playerObserver && player !== observedPlayer) {
      if (observedPlayer) playerObserver.unobserve(observedPlayer);
      playerObserver.observe(player);
      observedPlayer = player;
    }

    // Mid-drag the glow goes back to being an ordinary part of the page. Lifting
    // it means pinning a large blurred element to fresh coordinates on every
    // frame, which is the most expensive thing on this path, and all it buys
    // during a drag is a bleed above the player that nobody is looking at.
    // `endResize` puts it back.
    if (resizing) {
      root.classList.remove(GLOW_CLASS);
      return;
    }

    const rect = player?.getBoundingClientRect();

    if (!rect || rect.width === 0) {
      root.classList.remove(GLOW_CLASS);
      return;
    }

    setMetric("--pake-glow-left", `${rect.left}px`);
    setMetric("--pake-glow-top", `${rect.top}px`);
    setMetric("--pake-glow-width", `${rect.width}px`);
    setMetric("--pake-glow-height", `${rect.height}px`);
    root.classList.add(GLOW_CLASS);
  }

  function syncWatchPage() {
    const watching = window.location.pathname === "/watch";
    document.documentElement.classList.toggle(WATCH_CLASS, watching);
    // Leaving a watch page mid-scroll would otherwise strand the last opacity.
    syncMastheadFade();
  }

  // Last value handed to the app, so a page that fires a burst of media events
  // does not rebuild the tray menu once per event.
  let reportedVideo = false;

  function syncVideoAvailability() {
    const available = onVideoPage() && Boolean(pictureInPictureVideo(false));
    if (available === reportedVideo) return;

    reportedVideo = available;
    invoke("set_video_available", { available }).catch(() => {});
  }

  // A video appearing is both a new tray item and a newly sized player.
  function onMediaChange() {
    syncVideoAvailability();
    scheduleSync();
  }

  // YouTube's layout is rendered after the first paint, so neither the row
  // height nor the scroll container exists when this script runs. Watch for it
  // in its own loop; `start` must run once only, or every retry would stack
  // another set of listeners.
  function watchYouTube(attempt = 0) {
    const bar = masthead();
    const app = document.querySelector("ytd-app");
    if (!bar || !app) {
      // Given up on rather than retried forever: this script runs on every page
      // in the webview, and a Google sign-in page has no masthead to wait for.
      if (attempt >= LAYOUT_ATTEMPTS) return;
      window.setTimeout(() => watchYouTube(attempt + 1), 500);
      return;
    }

    document.documentElement.classList.add(YOUTUBE_CLASS);

    // Separate from the row observer below because the player comes and goes
    // with the page and resizes on its own in theatre mode. Created first so
    // the sync right after picks the player up on the way past.
    playerObserver = new ResizeObserver(scheduleSync);

    syncWatchPage();
    syncMetrics();

    // The masthead sets the row height, and it is the only thing watched for
    // it. ytd-app used to be watched as well, which fed itself: the height this
    // writes is what positions and sizes ytd-app, so every notification
    // produced another one, and a resize turned into a loop the browser had to
    // break for itself once a frame.
    const observer = new ResizeObserver(scheduleSync);
    observer.observe(bar);

    app.addEventListener("scroll", syncMastheadFade, { passive: true });
    window.addEventListener("yt-navigate-finish", () => {
      syncWatchPage();
      syncVideoAvailability();
      scheduleSync();
    });

    // Media events do not bubble, so these are caught on the way down. Between
    // them and the navigation above, every way a video appears or goes is
    // covered: loading one, swapping the source, and leaving the page.
    for (const type of ["loadeddata", "emptied", "ended"]) {
      document.addEventListener(type, onMediaChange, true);
    }

    syncVideoAvailability();
  }

  function start() {
    installStyles();
    installControls();
    syncMetrics();

    document.addEventListener("mousedown", onMouseDown, true);
    document.addEventListener("dblclick", onDoubleClick, true);

    // The floating window is WebView2's, so it turns up on the taskbar wearing
    // the runtime's icon. Caught here rather than on the request itself so the
    // right-click menu's own picture-in-picture is covered too; the event does
    // not bubble, hence the capture.
    document.addEventListener(
      "enterpictureinpicture",
      () => invoke("picture_in_picture_opened").catch(() => {}),
      true,
    );

    // Whether the window is maximized is a round trip to the app, and it can
    // only have changed once the drag is over, so it is asked for then rather
    // than on every frame of one.
    window.addEventListener(
      "resize",
      () => {
        beginResize();
        scheduleSync();
      },
      { passive: true },
    );

    watchYouTube();
  }

  if (document.body) {
    start();
  } else {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  }
})();

