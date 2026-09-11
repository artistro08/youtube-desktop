// YouTube notification bridge.
//
// YouTube delivers desktop notifications through the Web Push API, which the
// system WebView has no push service for, so the site's own notifications never
// reach a packaged app. Instead we poll the same InnerTube endpoints the bell
// icon uses (with the page's own session), diff against what has already been
// announced, and hand each new item to the native notifier. The toast carries
// the item's URL on the app's own youtube:// scheme, so clicking it opens the
// page here whether the app is still running or was closed in the meantime.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  if (!/(^|\.)youtube\.com$/i.test(window.location.hostname)) return;
  // Injected into every frame, and YouTube's live chat is an iframe: without
  // this the chat panel gets its own copy of all of this.
  if (window.top !== window.self) return;
  if (window.__pakeYoutubeNotifications) return;
  window.__pakeYoutubeNotifications = true;

  const POLL_INTERVAL_MS = 60_000;
  const FIRST_POLL_DELAY_MS = 15_000;
  const SEEN_STORAGE_KEY = "pakeSeenNotificationIds";
  const MAX_SEEN_IDS = 300;
  // A cold start must not replay the whole inbox as toasts.
  let seedingRun = true;

  // Seen ids
  function loadSeenIds() {
    try {
      const raw = window.localStorage.getItem(SEEN_STORAGE_KEY);
      const parsed = raw ? JSON.parse(raw) : [];
      return new Set(Array.isArray(parsed) ? parsed : []);
    } catch (error) {
      return new Set();
    }
  }

  function saveSeenIds(seenIds) {
    try {
      const trimmed = [...seenIds].slice(-MAX_SEEN_IDS);
      window.localStorage.setItem(SEEN_STORAGE_KEY, JSON.stringify(trimmed));
    } catch (error) {
      // Private mode or a full quota: dedupe degrades to this session only.
    }
  }

  // InnerTube request signing
  //
  // Signed-in youtubei calls are authorized with a SAPISIDHASH built from the
  // session cookie, the current timestamp, and the page origin. Cookies alone
  // are accepted on some endpoints and rejected on others, so always send it
  // when the cookie is present.
  async function buildAuthorizationHeader() {
    const cookieMatch = document.cookie.match(
      /(?:^|;\s*)(?:__Secure-3PAPISID|SAPISID)=([^;]+)/,
    );
    if (!cookieMatch) return null;

    const timestamp = Math.floor(Date.now() / 1000);
    const payload = `${timestamp} ${cookieMatch[1]} ${window.location.origin}`;
    const digest = await crypto.subtle.digest(
      "SHA-1",
      new TextEncoder().encode(payload),
    );
    const hash = [...new Uint8Array(digest)]
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join("");

    return `SAPISIDHASH ${timestamp}_${hash}`;
  }

  function readConfig(key) {
    try {
      return window.ytcfg?.get?.(key);
    } catch (error) {
      return undefined;
    }
  }

  async function callInnerTube(endpoint, body) {
    const apiKey = readConfig("INNERTUBE_API_KEY");
    const context = readConfig("INNERTUBE_CONTEXT");
    if (!apiKey || !context) return null;

    const headers = {
      "Content-Type": "application/json",
      "X-Goog-AuthUser": "0",
      "X-Youtube-Client-Name": String(
        readConfig("INNERTUBE_CONTEXT_CLIENT_NAME") || 1,
      ),
      "X-Youtube-Client-Version": String(
        readConfig("INNERTUBE_CLIENT_VERSION") || "",
      ),
    };

    const visitorData = readConfig("VISITOR_DATA");
    if (visitorData) headers["X-Goog-Visitor-Id"] = visitorData;

    const authorization = await buildAuthorizationHeader();
    if (authorization) headers["Authorization"] = authorization;

    const response = await fetch(
      `/youtubei/v1/${endpoint}?key=${encodeURIComponent(apiKey)}&prettyPrint=false`,
      {
        method: "POST",
        credentials: "include",
        headers,
        body: JSON.stringify({ context, ...body }),
      },
    );

    return response.ok ? response.json() : null;
  }

  // Response parsing
  function extractText(node) {
    if (!node) return "";
    if (typeof node.simpleText === "string") return node.simpleText;
    if (Array.isArray(node.runs)) {
      return node.runs.map((run) => run.text || "").join("");
    }
    return "";
  }

  // The notification menu is delivered as a popup action wrapping a list of
  // sections; walk it defensively because the shape shifts between rollouts.
  function collectNotifications(payload) {
    const items =
      payload?.actions
        ?.flatMap(
          (action) =>
            action?.openPopupAction?.popup?.multiPageMenuRenderer?.sections ??
            [],
        )
        ?.flatMap(
          (section) =>
            section?.multiPageMenuNotificationSectionRenderer?.items ?? [],
        ) ?? [];

    return items
      .map((item) => item?.notificationRenderer)
      .filter(Boolean)
      .map((renderer) => {
        // Comment notifications (replies, hearts) carry no web URL, only the
        // ids YouTube's own inbox uses to open the comment. The watch page
        // accepts the same pair as `v` and `lc`, so build that link instead
        // of leaving the toast with nowhere to go.
        const comment = renderer.navigationEndpoint?.getCommentsFromInboxCommand;
        const path =
          renderer.navigationEndpoint?.commandMetadata?.webCommandMetadata
            ?.url ||
          (comment?.videoId
            ? `/watch?v=${encodeURIComponent(comment.videoId)}&lc=${encodeURIComponent(comment.linkedCommentId || "")}`
            : "");
        return {
          id:
            renderer.notificationId ||
            path ||
            extractText(renderer.shortMessage),
          read: renderer.read === true,
          title: extractText(renderer.shortMessage) || "YouTube",
          body: extractText(renderer.sentTimeText),
          url: path ? new URL(path, window.location.origin).href : "",
        };
      })
      .filter((notification) => notification.id);
  }

  // Polling
  async function pollNotifications() {
    if (readConfig("LOGGED_IN") === false) return;

    // The inbox is fetched outright on every poll. It used to be gated on
    // `notification/get_unseen_count`, but that endpoint no longer reports a
    // top-level `unseenCount`, so the gate read 0 forever and nothing past the
    // seeding pass ever ran. Once a minute, the menu request is cheap enough.
    const menu = await callInnerTube("notification/get_notification_menu", {
      notificationsMenuRequestType: "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX",
    });
    const notifications = collectNotifications(menu);
    if (!notifications.length) {
      seedingRun = false;
      return;
    }

    const seenIds = loadSeenIds();
    for (const notification of notifications) {
      if (seenIds.has(notification.id)) continue;
      seenIds.add(notification.id);

      if (seedingRun || notification.read) continue;

      invoke("youtube_notify", {
        params: {
          title: notification.title,
          body: notification.body,
          url: notification.url,
        },
      }).catch((error) => {
        console.warn("[Pake] Failed to show YouTube notification:", error);
      });
    }

    saveSeenIds(seenIds);
    seedingRun = false;
  }

  function safePoll() {
    pollNotifications().catch((error) => {
      console.warn("[Pake] YouTube notification poll failed:", error);
    });
  }

  window.setTimeout(safePoll, FIRST_POLL_DELAY_MS);
  window.setInterval(safePoll, POLL_INTERVAL_MS);
})();

// Outbound links.
//
// YouTube funnels links in descriptions and comments through its own
// interstitial: `https://www.youtube.com/redirect?q=<destination>`. That is a
// youtube.com address, so the app's internal-URL rule quite correctly keeps it
// in the window, and the destination then loads in-app once YouTube redirects.
// Unwrap the real target on click and hand it to the system browser instead.
//
// The listener is registered at script evaluation, ahead of the generic Pake
// link handler that binds on DOMContentLoaded, so this runs first.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  if (!/(^|\.)youtube\.com$/i.test(window.location.hostname)) return;
  if (window.__pakeOutboundLinks) return;
  window.__pakeOutboundLinks = true;

  function outboundTarget(href) {
    try {
      const url = new URL(href, window.location.href);
      if (!/(^|\.)youtube\.com$/i.test(url.hostname)) return null;
      if (url.pathname !== "/redirect") return null;

      const target = url.searchParams.get("q");
      if (!target) return null;

      const parsed = new URL(target);
      return parsed.protocol === "https:" || parsed.protocol === "http:"
        ? parsed.href
        : null;
    } catch (error) {
      return null;
    }
  }

  document.addEventListener(
    "click",
    (event) => {
      const anchor = event.target?.closest?.("a");
      if (!anchor?.href) return;

      const target = outboundTarget(anchor.href);
      if (!target) return;

      event.preventDefault();
      event.stopImmediatePropagation();
      invoke("plugin:shell|open", { path: target }).catch((error) => {
        console.warn("[Pake] Failed to open an outbound link:", error);
      });
    },
    true,
  );
})();

// Resume where the user left off.
//
// The app reopens on the last page it was on, which means the page has to keep
// telling it where that is. YouTube is a single-page app, so most navigation
// never triggers a document load the Rust side could observe: the location is
// reported on YouTube's own navigation event, on history changes, and on a slow
// poll that catches whatever those two miss.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  if (!/(^|\.)youtube\.com$/i.test(window.location.hostname)) return;
  // Injected into every frame, and YouTube's live chat is an iframe: without
  // this the chat panel gets its own copy of all of this.
  if (window.top !== window.self) return;
  if (window.__pakeResumeBridge) return;
  window.__pakeResumeBridge = true;

  const POLL_INTERVAL_MS = 5_000;
  let lastReported = "";

  function reportLocation() {
    const url = window.location.href;
    if (url === lastReported || !url.startsWith("https://")) return;
    lastReported = url;

    invoke("save_last_url", { url }).catch((error) => {
      console.warn("[Pake] Failed to remember the current page:", error);
    });
  }

  window.addEventListener("yt-navigate-finish", reportLocation);
  window.addEventListener("popstate", reportLocation);
  window.addEventListener("pagehide", reportLocation);
  window.setInterval(reportLocation, POLL_INTERVAL_MS);
  reportLocation();
})();

// Windows media controls bridge.
//
// The app publishes its own System Media Transport Controls session (see
// `app/media.rs`) instead of letting WebView2 do it, because WebView2's session
// is owned by a process Windows cannot name. That means the page has to report
// what is playing, and has to carry out the transport buttons itself.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  if (!/(^|\.)youtube\.com$/i.test(window.location.hostname)) return;
  // Injected into every frame, and YouTube's live chat is an iframe: without
  // this the chat panel gets its own copy of all of this.
  if (window.top !== window.self) return;
  if (window.__pakeMediaBridge) return;
  window.__pakeMediaBridge = true;

  // `navigator.mediaSession.metadata` has no change event, so the state is
  // polled at a rate that is cheap but still feels immediate on track changes.
  const POLL_INTERVAL_MS = 3_000;
  let lastReport = "";

  /// The video these controls speak for.
  ///
  /// Not simply the first `<video>` in the document: a watch page keeps several
  /// around for the sidebar's hover previews, and one of those can sort first
  /// with no source at all. Reporting that one says "nothing is playing", the
  /// session goes to Closed, and the media card disappears from the volume
  /// flyout — which is what picture-in-picture looked like it was breaking, as
  /// popping out rearranges the player and changes which video leads.
  ///
  /// The floating window's video wins outright while there is one, then a
  /// playing video, then any that has actually loaded something.
  function currentVideo() {
    const inPictureInPicture = document.pictureInPictureElement;
    if (inPictureInPicture instanceof HTMLVideoElement) {
      return inPictureInPicture;
    }

    const videos = [...document.querySelectorAll("video")];
    return (
      videos.find((video) => !video.paused && !video.ended && video.readyState > 0) ||
      videos.find((video) => video.readyState > 0 || video.currentSrc) ||
      videos[0] ||
      null
    );
  }

  function largestArtwork(metadata) {
    const artwork = metadata?.artwork;
    if (!Array.isArray(artwork) || !artwork.length) return "";

    const area = (entry) => {
      const [width, height] = String(entry?.sizes || "0x0").split("x");
      return (Number(width) || 0) * (Number(height) || 0);
    };

    return [...artwork].sort((a, b) => area(b) - area(a))[0]?.src || "";
  }

  function readState() {
    const video = currentVideo();
    const metadata = navigator.mediaSession?.metadata;

    return {
      // currentSrc, not src: YouTube feeds the player through MediaSource and
      // leaves the src attribute empty, so testing src alone reports "nothing
      // playing" on the very page that is playing.
      has_media: Boolean(video && (video.currentSrc || video.src || video.readyState > 0)),
      playing: Boolean(video && !video.paused && !video.ended),
      title: metadata?.title || document.title.replace(/ - YouTube$/, ""),
      artist: metadata?.artist || "",
      artwork: largestArtwork(metadata),
    };
  }

  function report() {
    const state = readState();
    const fingerprint = JSON.stringify(state);
    if (fingerprint === lastReport) return;
    lastReport = fingerprint;

    invoke("update_media_state", { state }).catch((error) => {
      console.warn("[Pake] Failed to report media state:", error);
    });
  }

  // Carry out a transport button. YouTube's own controls are used where they
  // exist so playlist and chapter behaviour matches the player's.
  window.__pakeMediaCommand = (action) => {
    const video = currentVideo();

    if (action === "play") video?.play?.();
    else if (action === "pause") video?.pause?.();
    else if (action === "next") {
      // The player's own button carries playlist and autoplay behaviour, but
      // it is not always reachable — while the video is in the floating window
      // the page swaps the control bar out for a placeholder. Falling through
      // to the end of the video hands YouTube the same job.
      const next = document.querySelector(".ytp-next-button");
      if (next) next.click();
      else if (video?.duration) video.currentTime = video.duration;
    } else if (action === "previous") {
      const previous = document.querySelector(".ytp-prev-button");
      // Without a previous entry, restart the current item the way a player
      // would rather than doing nothing at all.
      if (previous) previous.click();
      else if (video) video.currentTime = 0;
    }

    report();
  };

  // Media events do not bubble, so these listen in the capture phase.
  for (const event of ["play", "pause", "ended", "loadedmetadata", "emptied"]) {
    document.addEventListener(event, report, true);
  }
  window.addEventListener("yt-navigate-finish", () =>
    window.setTimeout(report, 500),
  );
  window.setInterval(report, POLL_INTERVAL_MS);
  report();
})();

// App settings entry in the avatar menu.
//
// Turning the tray icon off removes the only place the app could otherwise be
// configured from, so the switch has to live somewhere the user can always
// reach: directly under YouTube's own "Settings" item in the avatar menu.
//
// Two constraints shape this code:
//   - YouTube serves `require-trusted-types-for 'script'`, so assigning to
//     innerHTML throws and takes the whole injection down with it. Every node
//     here is built with createElement / createElementNS instead.
//   - `ytd-compact-link-renderer` paints its icon and label from Polymer state
//     in a shadow root, so cloning YouTube's own row yields an empty shell.
//     The row is built from scratch, inherits the menu's own text colour, and
//     takes its remaining colours from the page's `dark` attribute.

(function () {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  if (!/(^|\.)youtube\.com$/i.test(window.location.hostname)) return;
  // Injected into every frame, and YouTube's live chat is an iframe: without
  // this the chat panel gets its own copy of all of this.
  if (window.top !== window.self) return;
  if (window.__pakeSettingsMenu) return;
  window.__pakeSettingsMenu = true;

  const MENU_ITEM_ID = "pake-app-settings-item";
  const MENU_STYLE_ID = "pake-app-settings-style";
  const DIALOG_HOST_ID = "pake-app-settings-dialog";
  // YouTube's own Settings entry, which this one is inserted after.
  const SETTINGS_LINK_SELECTOR = 'a[href^="/account"]';
  // Material "tune" glyph: reads as settings without colliding with YouTube's
  // own gear right above it.
  const ICON_PATH =
    "M3 17v2h6v-2H3zM3 5v2h10V5H3zm10 16v-2h8v-2h-8v-2h-2v6h2zM7 9v2H3v2h4v2h2V9H7zm14 4v-2H11v2h10zm-6-4h2V7h4V5h-4V3h-2v6z";
  // YouTube's own dialog close glyph, copied from the share dialog's button so
  // this dialog dismisses with the same icon as the rest. It is the rounded,
  // heavier X of the current icon set, not the thin one YouTube used before.
  const CLOSE_PATH =
    "M17.293 5.293 12 10.586 6.707 5.293a1 1 0 10-1.414 1.414L10.586 12l-5.293 5.293a1 1 0 001.414 1.414L12 13.414l5.293 5.293a1 1 0 001.414-1.414L13.414 12l5.293-5.293a1 1 0 10-1.414-1.414Z";
  const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

  // DOM helpers (Trusted Types safe)
  function el(tag, options = {}, children = []) {
    const node = document.createElement(tag);
    if (options.id) node.id = options.id;
    if (options.className) node.className = options.className;
    if (options.text) node.textContent = options.text;
    for (const [name, value] of Object.entries(options.attrs || {})) {
      node.setAttribute(name, value);
    }
    for (const child of children) node.appendChild(child);
    return node;
  }

  function icon(size, shape = ICON_PATH) {
    const svg = document.createElementNS(SVG_NAMESPACE, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", String(size));
    svg.setAttribute("height", String(size));
    svg.setAttribute("aria-hidden", "true");
    // Inline, because YouTube ships document-level `svg { fill: ... }` rules
    // that outrank an injected stylesheet and repaint the glyph near-black.
    svg.style.fill = "currentColor";

    const path = document.createElementNS(SVG_NAMESPACE, "path");
    path.setAttribute("d", shape);
    svg.appendChild(path);

    return svg;
  }

  function styleElement(id, css) {
    const style = document.createElement("style");
    style.id = id;
    // textContent is not a Trusted Types sink, unlike innerHTML.
    style.textContent = css;
    return style;
  }

  // Menu row
  function ensureMenuStyle() {
    if (document.getElementById(MENU_STYLE_ID)) return;

    document.head.appendChild(
      styleElement(
        MENU_STYLE_ID,
        `
          #${MENU_ITEM_ID} {
            display: flex;
            align-items: center;
            gap: 16px;
            box-sizing: border-box;
            min-height: 40px;
            padding: 0 36px 0 16px;
            cursor: pointer;
            user-select: none;
            font-family: "Roboto", "Arial", sans-serif;
            font-size: 14px;
            line-height: 20px;
            /* Stated outright, keyed off the page's theme attribute. Neither of
               the alternatives works here: YouTube's --yt-spec-* properties do
               not resolve at this point in the tree, and inheriting picks up
               the container's dimmed colour rather than the primary text colour
               that ytd-compact-link-renderer sets on itself. The icon follows
               via currentColor. */
            color: #0f0f0f;
          }
          html[dark] #${MENU_ITEM_ID} {
            color: #f1f1f1;
          }
          #${MENU_ITEM_ID}:hover,
          #${MENU_ITEM_ID}:focus-visible {
            background-color: rgba(0, 0, 0, 0.05);
            outline: none;
          }
          html[dark] #${MENU_ITEM_ID}:hover,
          html[dark] #${MENU_ITEM_ID}:focus-visible {
            background-color: rgba(255, 255, 255, 0.1);
          }
          #${MENU_ITEM_ID} svg,
          #${MENU_ITEM_ID} svg path {
            flex: 0 0 auto;
            width: 24px;
            height: 24px;
            fill: currentColor !important;
          }
        `,
      ),
    );
  }

  function buildMenuItem() {
    const item = el(
      "div",
      { id: MENU_ITEM_ID, attrs: { role: "menuitem", tabindex: "0" } },
      [icon(24), el("span", { text: "App settings" })],
    );

    const activate = (event) => {
      event.preventDefault();
      event.stopPropagation();
      closeAvatarMenu(item);
      openDialog();
    };

    item.addEventListener("click", activate);
    item.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") activate(event);
    });

    return item;
  }

  function closeAvatarMenu(node) {
    const dropdown = node.closest("tp-yt-iron-dropdown");
    if (dropdown && typeof dropdown.close === "function") {
      dropdown.close();
      return;
    }
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
  }

  function injectMenuItem(container) {
    const settingsLink = container.querySelector(SETTINGS_LINK_SELECTOR);
    const settingsItem = settingsLink?.closest("ytd-compact-link-renderer");
    if (!settingsItem || !settingsItem.parentElement) return;
    if (settingsItem.parentElement.querySelector(`#${MENU_ITEM_ID}`)) return;

    ensureMenuStyle();
    settingsItem.after(buildMenuItem());
  }

  // Dialog
  function dialogStyles(dark) {
    // Colours are derived from the page's theme attribute rather than from
    // YouTube's --yt-spec-* properties, which do not resolve reliably inside a
    // shadow root: a var() that silently falls back paints light-theme colours
    // onto a dark page.
    const surface = dark ? "#282828" : "#ffffff";
    const primary = dark ? "#f1f1f1" : "#0f0f0f";
    const secondary = dark ? "#aaaaaa" : "#606060";
    const divider = dark ? "rgba(255, 255, 255, 0.1)" : "rgba(0, 0, 0, 0.1)";
    const action = dark ? "#3ea6ff" : "#065fd4";
    // YouTube's own switches keep one grey bar in both states and move a thumb
    // across it, grey when off and themed blue when on. Only the thumb changes.
    const track = dark ? "rgba(170, 170, 170, 0.5)" : "rgba(96, 96, 96, 0.38)";
    const thumbOff = dark ? "#909090" : "#fafafa";

    return `
      .backdrop {
        position: fixed;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        background: rgba(0, 0, 0, 0.5);
        z-index: 2147483647;
      }
      .panel {
        width: 420px;
        max-width: calc(100vw - 32px);
        background: ${surface};
        color: ${primary};
        border-radius: 12px;
        box-shadow: 0 4px 32px rgba(0, 0, 0, 0.5);
        font-family: "Roboto", "Arial", sans-serif;
        font-size: 14px;
        line-height: 20px;
        overflow: hidden;
      }
      .header {
        display: flex;
        align-items: center;
        gap: 16px;
        padding: 16px 24px;
        border-bottom: 1px solid ${divider};
      }
      .header svg {
        flex: 0 0 auto;
        fill: currentColor;
      }
      .header h2 {
        flex: 1 1 auto;
        margin: 0;
        font-size: 16px;
        font-weight: 500;
      }
      .close {
        display: flex;
        flex: 0 0 auto;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        /* Pulled into the header's own padding so the glyph lines up with the
           title rather than sitting a button's worth of whitespace inside it. */
        margin: -8px -8px -8px 0;
        padding: 0;
        border: none;
        border-radius: 50%;
        background: none;
        color: inherit;
        cursor: pointer;
      }
      .close:hover {
        background: ${dark ? "rgba(255, 255, 255, 0.1)" : "rgba(0, 0, 0, 0.05)"};
      }
      .rows {
        padding: 8px 24px 16px;
      }
      .row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 24px;
        padding: 16px 0;
      }
      .row-label {
        font-weight: 500;
      }
      .section {
        margin: 0 -24px;
        padding: 16px 24px 0;
        border-top: 1px solid ${divider};
      }
      .section h3 {
        margin: 0;
        font-size: 14px;
        font-weight: 500;
        color: ${secondary};
      }
      .row-hint {
        margin-top: 4px;
        font-size: 12px;
        line-height: 18px;
        color: ${secondary};
      }
      .switch {
        position: relative;
        flex: 0 0 auto;
        width: 36px;
        height: 14px;
        padding: 0;
        border: none;
        border-radius: 7px;
        background: ${track};
        cursor: pointer;
        transition: background 0.15s ease;
      }
      .switch::after {
        content: "";
        position: absolute;
        top: -3px;
        left: 0;
        width: 20px;
        height: 20px;
        border-radius: 50%;
        background: ${thumbOff};
        box-shadow: 0 1px 3px rgba(0, 0, 0, 0.4);
        transition: transform 0.15s ease, background 0.15s ease;
      }
      .switch[aria-checked="true"]::after {
        transform: translateX(16px);
        background: ${action};
      }
      .footer {
        display: flex;
        justify-content: flex-end;
        padding: 0 16px 16px;
      }
      .done {
        background: none;
        border: none;
        border-radius: 18px;
        padding: 10px 16px;
        font-family: inherit;
        font-size: 14px;
        font-weight: 500;
        color: ${action};
        cursor: pointer;
      }
      .done:hover {
        background: ${divider};
      }
    `;
  }

  // Bound while the dialog is open, so it can be taken off again on every way
  // out rather than only on the one it handles itself.
  let escapeHandler = null;

  function closeDialog() {
    document.getElementById(DIALOG_HOST_ID)?.remove();

    if (escapeHandler) {
      document.removeEventListener("keydown", escapeHandler, true);
      escapeHandler = null;
    }
  }

  /// One labelled switch. The key names both the stored setting and the field
  /// to read it back from, so there is one name to keep in step, not two.
  function settingRow({ label, key, hintOn, hintOff }) {
    const hint = el("div", { className: "row-hint" });
    const toggle = el("button", {
      className: "switch",
      attrs: { role: "switch", "aria-checked": "false", "aria-label": label },
    });

    const render = (enabled) => {
      toggle.setAttribute("aria-checked", String(enabled));
      hint.textContent = enabled ? hintOn : hintOff;
    };

    toggle.addEventListener("click", () => {
      const enabled = toggle.getAttribute("aria-checked") !== "true";
      render(enabled);
      invoke("set_app_setting", { key, enabled }).catch((error) => {
        console.warn(`[Pake] Failed to change ${label}:`, error);
        // Leave the switch showing what the app actually does.
        render(!enabled);
      });
    });

    const element = el("div", { className: "row" }, [
      el("div", {}, [el("div", { className: "row-label", text: label }), hint]),
      toggle,
    ]);

    return { element, key, render };
  }

  /// The row that reports the installed version and looks for a newer one.
  ///
  /// Shaped like a setting row rather than styled separately, so it sits in the
  /// same list without a stylesheet of its own. The app owns everything about
  /// the check — where to look, which signature to trust, whether to install —
  /// so all this does is ask and show the answer.
  function updateCheckRow() {
    // Nothing is checked until the button is pressed. The app looks once on its
    // own after startup, and opening this dialog is not a reason to go back to
    // the network.
    const hint = el("div", {
      className: "row-hint",
      text: "Check whether a newer version is available.",
    });
    const button = el("button", { className: "done", text: "Check now" });

    const render = (status) => {
      if (!status) {
        hint.textContent = "The installed version could not be read.";
        return;
      }

      if (status.error) {
        hint.textContent = status.error;
        return;
      }

      hint.textContent = status.available
        ? `Version ${status.version} is available. You have ${status.currentVersion}.`
        : `Version ${status.currentVersion} is the latest.`;
    };

    const check = () => {
      button.disabled = true;
      hint.textContent = "Checking…";

      invoke("check_for_updates")
        .then(render)
        .catch((error) => {
          console.warn("[Pake] Failed to check for updates:", error);
          hint.textContent = "Could not check for updates.";
        })
        .finally(() => {
          button.disabled = false;
        });
    };

    button.addEventListener("click", check);

    const element = el("div", { className: "row" }, [
      el("div", {}, [el("div", { className: "row-label", text: "Updates" }), hint]),
      button,
    ]);

    return { element, check };
  }

  function openDialog() {
    closeDialog();

    const appRows = [
      settingRow({
        label: "Show tray icon",
        key: "tray_enabled",
        hintOn: "Closing the window keeps YouTube running in the tray.",
        hintOff: "With no tray icon, closing the window quits the app.",
      }),
      settingRow({
        label: "Continue where you left off",
        key: "resume_enabled",
        hintOn: "Opening the app returns you to the last page you were on.",
        hintOff: "The app always opens on the YouTube home page.",
      }),
      settingRow({
        label: "Pause Shorts when hidden",
        key: "pause_shorts",
        hintOn: "Minimizing or closing pauses a Short that is playing.",
        hintOff: "A Short keeps playing after the window is hidden.",
      }),
    ];

    const pipRows = [
      settingRow({
        label: "Picture-in-picture on minimize",
        key: "pip_on_minimize",
        hintOn: "Minimizing pops a playing video out into a floating window.",
        hintOff: "Minimizing leaves the video in the window.",
      }),
      settingRow({
        label: "Picture-in-picture on close",
        key: "pip_on_close",
        hintOn:
          "Closing to the tray pops a playing video out into a floating window.",
        hintOff: "Closing to the tray leaves the video in the window.",
      }),
      settingRow({
        label: "Include Shorts",
        key: "pip_on_shorts",
        hintOn: "Shorts pop out too.",
        hintOff: "Only full videos pop out; Shorts stay in the window.",
      }),
      settingRow({
        label: "Show on all desktops",
        key: "pip_all_desktops",
        hintOn: "The floating window follows you between virtual desktops.",
        hintOff: "The floating window stays on the desktop it opened on.",
      }),
    ];

    // Flat list for the settings round trip; the panel below groups them.
    const rows = [...appRows, ...pipRows];

    const updateRow = updateCheckRow();

    const done = el("button", { className: "done", text: "Done" });

    const close = el(
      "button",
      { className: "close", attrs: { "aria-label": "Close" } },
      [icon(24, CLOSE_PATH)],
    );

    const panel = el(
      "div",
      { className: "panel", attrs: { role: "dialog", "aria-modal": "true" } },
      [
        el("div", { className: "header" }, [
          icon(24),
          el("h2", { text: "App settings" }),
          close,
        ]),
        el("div", { className: "rows" }, [
          ...appRows.map((row) => row.element),
          el("div", { className: "section" }, [
            el("h3", { text: "Picture in Picture Settings" }),
          ]),
          ...pipRows.map((row) => row.element),
          el("div", { className: "section" }, [el("h3", { text: "About" })]),
          updateRow.element,
        ]),
        el("div", { className: "footer" }, [done]),
      ],
    );

    const backdrop = el("div", { className: "backdrop" }, [panel]);

    const host = el("div", { id: DIALOG_HOST_ID });
    const shadow = host.attachShadow({ mode: "open" });
    shadow.appendChild(
      styleElement(
        "styles",
        dialogStyles(document.documentElement.hasAttribute("dark")),
      ),
    );
    shadow.appendChild(backdrop);
    document.body.appendChild(host);

    invoke("get_app_settings")
      .then((settings) => {
        for (const row of rows) row.render(settings?.[row.key] === true);
      })
      .catch(() => {
        for (const row of rows) row.render(false);
      });

    done.addEventListener("click", closeDialog);
    close.addEventListener("click", closeDialog);
    backdrop.addEventListener("click", (event) => {
      if (event.target === backdrop) closeDialog();
    });
    // Unbound by closeDialog rather than by itself: closing with Done, the X or
    // the backdrop would otherwise leave it attached, and every reopen would
    // add another that fires on every Escape the user ever presses.
    escapeHandler = (event) => {
      if (event.key !== "Escape") return;
      closeDialog();
    };
    document.addEventListener("keydown", escapeHandler, true);
  }

  // The popup container is created once and only mutates while a menu opens or
  // closes, so observing it costs far less than watching the whole document.
  function watchPopupContainer() {
    const container = document.querySelector("ytd-popup-container");
    if (!container) {
      window.setTimeout(watchPopupContainer, 1_000);
      return;
    }

    new MutationObserver(() => injectMenuItem(container)).observe(container, {
      childList: true,
      subtree: true,
    });
  }

  // Copy the link — Ctrl+Shift+C
  const TOAST_HOST_ID = "pake-toast";
  // How long the snackbar stays up, matching YouTube's own.
  const TOAST_MS = 4_000;
  const TOAST_FADE_MS = 200;

  function toastStyles() {
    // YouTube's snackbar is near-black with white text in both themes, so it
    // does not follow the page theme the way the settings dialog does.
    return `
      .toast {
        position: fixed;
        left: 24px;
        bottom: 24px;
        max-width: calc(100vw - 48px);
        padding: 14px 16px;
        border-radius: 8px;
        background: #030303;
        color: #ffffff;
        font-family: "Roboto", "Arial", sans-serif;
        font-size: 14px;
        line-height: 20px;
        box-shadow: 0 4px 16px rgba(0, 0, 0, 0.5);
        /* Above YouTube's dialogs, below nothing: this is the last thing drawn. */
        z-index: 2147483647;
        opacity: 0;
        transform: translateY(8px);
        transition: opacity ${TOAST_FADE_MS}ms ease, transform ${TOAST_FADE_MS}ms ease;
      }
      .toast.visible {
        opacity: 1;
        transform: none;
      }
    `;
  }

  function showToast(message) {
    document.getElementById(TOAST_HOST_ID)?.remove();

    const toast = el("div", { className: "toast", text: message });
    const host = el("div", { id: TOAST_HOST_ID });
    const shadow = host.attachShadow({ mode: "open" });
    shadow.appendChild(styleElement("styles", toastStyles()));
    shadow.appendChild(toast);
    document.body.appendChild(host);

    // One frame in its starting state, or the transition has nothing to run from.
    requestAnimationFrame(() => toast.classList.add("visible"));

    window.setTimeout(() => {
      toast.classList.remove("visible");
      window.setTimeout(() => host.remove(), TOAST_FADE_MS);
    }, TOAST_MS);
  }

  /// The link YouTube itself would hand out for this page.
  ///
  /// The canonical link is the clean one: the address bar carries playlist
  /// position, the timestamp the video happens to be at, and whatever tracking
  /// parameters the page was opened with.
  function shareableUrl() {
    const canonical = document.querySelector('link[rel="canonical"]');
    return canonical?.href || window.location.href;
  }

  function onCopyShortcut(event) {
    if (!event.ctrlKey || !event.shiftKey || event.altKey) return;
    // code first, since with Shift held key reads "C" and a non-QWERTY layout
    // moves it; key as a fallback, because a synthesised keypress carries no
    // scan code and so no code at all.
    if (event.code !== "KeyC" && event.key?.toLowerCase() !== "c") return;

    event.preventDefault();
    event.stopPropagation();

    navigator.clipboard
      .writeText(shareableUrl())
      .then(() => showToast("Link copied to clipboard"))
      .catch((error) => {
        console.warn("[Pake] Could not copy the link:", error);
        showToast("Couldn't copy the link");
      });
  }

  // Capture, so the shortcut works with the focus anywhere on the page.
  document.addEventListener("keydown", onCopyShortcut, true);


  watchPopupContainer();
})();



