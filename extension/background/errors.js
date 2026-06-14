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

// Cap a page-influenced string for a one-line, agent-facing error message — an
// iframe `src`, a frame name, or a browser-supplied fetch reason can be huge or
// carry spoof characters. Matches the Rust `line_safe_clip` EXACTLY (the wording
// crosses the parity contract): control / bidi / zero-width chars → spaces FIRST
// (so a `\n` or U+202E can't forge or reorder a line), THEN clip on a codepoint
// boundary (a mid-surrogate cut would corrupt the JSON) at 200 + `…`.
function lineSafeClip(s) {
  const safe = Array.from(s || "").map((c) => {
    const cp = c.codePointAt(0);
    // The Rust `line_safe` spoof set: C0 (0x00..0x1F), DEL+C1 (0x7F..0x9F), the
    // bidi controls and zero-width formatters.
    const spoof =
      cp <= 0x1f ||
      (cp >= 0x7f && cp <= 0x9f) ||
      (cp >= 0x200b && cp <= 0x200f) ||
      (cp >= 0x202a && cp <= 0x202e) ||
      (cp >= 0x2066 && cp <= 0x2069) ||
      cp === 0x061c ||
      cp === 0xfeff;
    return spoof ? " " : c;
  });
  return safe.length > 200 ? safe.slice(0, 200).join("") + "…" : safe.join("");
}

export { err, exceptionErr, lineSafeClip, noPageErr, otherErr, timeoutErr, topErr };
