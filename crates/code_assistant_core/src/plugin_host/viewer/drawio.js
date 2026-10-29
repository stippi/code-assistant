// The `drawio` protocol: draw.io's embed mode (`embed=1&proto=json`).
//
// Embedded in a frame, draw.io talks to its parent with postMessage. Here
// it is the top window, its own parent: messages it posts to itself are
// taken off before delivery (its events go to the app), and the app's
// actions are posted to the window, where draw.io accepts them as coming
// from its parent.
(() => {
  if (window.top !== window) return;
  const send = (message) => window.__codeAssistantHost(JSON.stringify(message));
  const file = __FILE__;
  const isSvg = /\.svg$/i.test(file.path);
  const deliver = window.postMessage.bind(window);
  const post = (action) => deliver(JSON.stringify(action), "*");
  let text = null;
  let initialized = false;

  // The diagram XML of the file: a .drawio.svg carries it in its `content`.
  const diagramOf = (source) => {
    if (!isSvg || !source.trim()) return source;
    const svg = new DOMParser().parseFromString(source, "image/svg+xml").documentElement;
    return (svg && svg.getAttribute("content")) || "";
  };
  const load = () => post({ action: "load", xml: diagramOf(text || ""), autosave: 1 });
  const decodeDataUri = (uri) => {
    const comma = uri.indexOf(",");
    const body = uri.slice(comma + 1);
    if (!/;base64/.test(uri.slice(0, comma))) return decodeURIComponent(body);
    const bytes = Uint8Array.from(atob(body), (c) => c.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  };
  const onEvent = (message) => {
    switch (message.event) {
      case "init":
        initialized = true;
        if (text !== null) load();
        break;
      case "autosave":
      case "save":
        if (isSvg) post({ action: "export", format: "xmlsvg", xml: message.xml });
        else send({ type: "text", text: message.xml });
        break;
      case "export":
        if (isSvg && message.data) send({ type: "text", text: decodeDataUri(message.data) });
        break;
    }
  };

  window.postMessage = function (message, ...rest) {
    if (typeof message === "string") {
      let parsed = null;
      try { parsed = JSON.parse(message); } catch (e) { /* not ours */ }
      if (parsed && typeof parsed.event === "string") {
        onEvent(parsed);
        return;
      }
    }
    return deliver(message, ...rest);
  };
  window.__codeAssistant = {
    receiveText(next) {
      text = next;
      if (initialized) load();
    },
  };
  send({ type: "ready" });
})();
