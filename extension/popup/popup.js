const dot = document.getElementById("dot");
const statusText = document.getElementById("statusText");
const info = document.getElementById("info");

// Check connection by sending a status request to service worker
chrome.runtime.sendMessage({ type: "status" }, (response) => {
  if (chrome.runtime.lastError || !response) {
    dot.className = "dot off";
    statusText.textContent = "Not connected";
    info.innerHTML = '<p>Run <code>webpilot install</code> in terminal</p>';
  } else {
    dot.className = "dot on";
    statusText.textContent = "Connected";
    info.innerHTML = `<p>v${chrome.runtime.getManifest().version}</p>`;
  }
});
