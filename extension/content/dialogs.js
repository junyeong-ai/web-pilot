// Answers the page's alert/confirm/prompt in place of the native modal, which
// would hold the page until someone answered it, and records each one for the
// next command to report. MAIN world: the page calls these functions itself.
// Both modes run this file: headless registers it on every new document,
// browser mode injects it into the pinned tab's documents as they commit.
//
// Best-effort, like the monitors: a page can restore the native functions or
// write into the record, so a read validates each entry's shape and an agent
// treats an empty record as no evidence either way.
(() => {
  if (window.__webpilot_dialogs_patched === true) return;
  window.__webpilot_dialogs_patched = true;
  window.__webpilot_dialogs = [];
  if (typeof window.__webpilot_accept_dialogs !== "boolean") {
    window.__webpilot_accept_dialogs = false;
  }
  // A page in a dialog loop must not grow the record without bound.
  const RECORD_CAP = 100;
  const record = (type, message, accepted) => {
    const log = window.__webpilot_dialogs;
    if (Array.isArray(log) && log.length < RECORD_CAP) {
      // WebIDL: a missing message is the empty string, anything else its string.
      log.push({ type, message: message === undefined ? "" : String(message), accepted });
    }
  };
  window.alert = function alert(message) {
    record("alert", message, true);
  };
  window.confirm = function confirm(message) {
    const accepted = window.__webpilot_accept_dialogs === true;
    record("confirm", message, accepted);
    return accepted;
  };
  // An accepted prompt returns its default stringified — `prompt(m, 0)` gives
  // "0" and `prompt(m, null)` gives "null" — and only a missing default gives "".
  window.prompt = function prompt(message, value) {
    const accepted = window.__webpilot_accept_dialogs === true;
    record("prompt", message, accepted);
    if (!accepted) return null;
    return value === undefined ? "" : String(value);
  };
})();
