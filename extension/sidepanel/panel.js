const log = document.getElementById("log");

chrome.runtime.onMessage.addListener((msg) => {
  if (msg.type === "log") {
    const entry = document.createElement("div");
    entry.className = "entry";
    entry.textContent = `${new Date().toLocaleTimeString()} ${msg.text}`;
    log.appendChild(entry);
    log.scrollTop = log.scrollHeight;
  }
});
