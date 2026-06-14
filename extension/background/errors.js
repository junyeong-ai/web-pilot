// Error envelopes — the wire shape of `WebPilotError` (code + message + data).
// Mirrors crates/webpilot/src/error.rs.

function err(code, message, data) {
  return { code, message, ...(data || {}) };
}
const otherErr = (msg) => err("Other", msg);
// Preserve a thrown error's typed `code` (e.g. BridgeUnavailable → exit 3)
// and its wire fields (`e.data`, e.g. TabNotFound's tab_id) instead of
// collapsing every exception to Other (exit 1).
const exceptionErr = (e) =>
  e?.code ? err(e.code, e.message || String(e), e.data) : otherErr(e?.message || String(e));
const timeoutErr = (kind, elapsed_ms) => err("Timeout", `${kind} timed out`, { kind, elapsed_ms });
const noPageErr = () => err("NoPage", "No web page open");

function topErr(error) {
  return { type: "Error", error };
}

export { err, exceptionErr, noPageErr, otherErr, timeoutErr, topErr };
