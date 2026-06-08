// // Command router: one exhaustive dispatch over the wire `Command` tags —
// // the JS twin of LocalTransport::send, guarded by tests/browser_parity.rs.

import { exceptionErr, otherErr, topErr } from "./errors.js";
import { RESTORED } from "./session.js";
import { handleAction } from "./action.js";
import { handleCapture } from "./capture.js";
import { handleDomGet, handleDomSet, handleEval, handleFetch, handleWait } from "./query.js";
import { handleConsoleClear, handleConsoleRead, handleConsoleStart, handleCookieDelete, handleCookieList, handleCookieSet, handleNetworkClear, handleNetworkRead, handleNetworkStart, handleSessionExport, handleSessionImport } from "./state.js";
import { handleFrameList, handleFrameSwitch, handleStatus, handleTabClose, handleTabList, handleTabNew, handleTabSwitch } from "./browser.js";

// ── Command dispatch ───────────────────────────────────────────────────────

async function processCommand(id, command, port) {
  // Never act on un-restored state after a service-worker restart.
  await RESTORED;
  // Reply to the port that delivered this request, not the mutable global
  // `nmPort`. A disconnected port throws — drop the reply rather than risk
  // landing it on a reconnected host (the originating CLI already failed on its
  // dead socket).
  const send = (result) => {
    try {
      port.postMessage({ id, result });
    } catch {}
  };
  try {
    let result;
    switch (command.type) {
      case "Capture":
        result = await handleCapture(command);
        break;

      case "Action":
        result = await handleAction(command);
        break;

      case "Status":
        result = await handleStatus();
        break;

      case "TabList":
        result = await handleTabList();
        break;

      case "TabSwitch":
        result = await handleTabSwitch(command.tab_id);
        break;

      case "TabNew":
        result = await handleTabNew(command.url);
        break;

      case "TabClose":
        result = await handleTabClose(command.tab_id);
        break;

      case "Eval":
        result = await handleEval(command);
        break;

      case "Wait":
        result = await handleWait(command);
        break;

      case "DomSet":
        result = await handleDomSet(command);
        break;

      case "DomGet":
        result = await handleDomGet(command);
        break;

      case "Fetch":
        result = await handleFetch(command);
        break;

      case "FrameList":
        result = await handleFrameList();
        break;

      case "FrameSwitch":
        result = await handleFrameSwitch(command.selector);
        break;

      case "CookieList":
        result = await handleCookieList(command.url);
        break;

      case "CookieSet":
        result = await handleCookieSet(command);
        break;

      case "CookieDelete":
        result = await handleCookieDelete(command);
        break;

      case "ConsoleStart":
        result = await handleConsoleStart();
        break;

      case "ConsoleRead":
        result = await handleConsoleRead(command.since);
        break;

      case "ConsoleClear":
        result = await handleConsoleClear();
        break;

      case "NetworkStart":
        result = await handleNetworkStart();
        break;

      case "NetworkRead":
        result = await handleNetworkRead(command.since);
        break;

      case "NetworkClear":
        result = await handleNetworkClear();
        break;

      case "SessionExport":
        result = await handleSessionExport();
        break;

      case "SessionImport":
        result = await handleSessionImport(command.data);
        break;

      case "Ping":
        result = { type: "Pong" };
        break;

      default:
        result = topErr(otherErr(`Unknown command: ${command.type}`));
    }

    send(result);
  } catch (e) {
    send(topErr(exceptionErr(e)));
  }
}

export { processCommand };
