// The `host-api` protocol: the page uses `window.codeAssistant`.
//
//   codeAssistant.onText(text => …)   the file's text, at once and on every change
//   codeAssistant.setText(text)       write the file (saved like an edit in the editor)
//   codeAssistant.path, .relativePath the file shown
(() => {
  if (window.top !== window) return;
  const send = (message) => window.__codeAssistantHost(JSON.stringify(message));
  const file = __FILE__;
  let text = null;
  const listeners = [];
  window.__codeAssistant = {
    receiveText(next) {
      text = next;
      for (const listener of listeners) {
        try { listener(next); } catch (e) { console.error(e); }
      }
    },
  };
  window.codeAssistant = {
    path: file.path,
    relativePath: file.relativePath,
    get text() { return text; },
    onText(listener) {
      listeners.push(listener);
      if (text !== null) listener(text);
    },
    setText(next) {
      text = String(next);
      send({ type: "text", text });
    },
  };
  send({ type: "ready" });
})();
