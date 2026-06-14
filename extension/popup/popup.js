const dot = document.getElementById("dot");
const statusText = document.getElementById("statusText");
const info = document.getElementById("info");

chrome.runtime.sendMessage({ type: "status" }, (response) => {
  if (chrome.runtime.lastError || !response?.connected) {
    dot.className = "dot off";
    statusText.textContent = "Not connected";
    info.innerHTML = '<p>Run <code>webpilot setup</code> and reload this extension, or diagnose with <code>webpilot --browser status</code>.</p>';
  } else {
    dot.className = "dot on";
    statusText.textContent = "Connected";
    info.innerHTML = `<p>v${chrome.runtime.getManifest().version}</p>`;
  }
});
