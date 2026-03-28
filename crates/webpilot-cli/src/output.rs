use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

pub fn detect_output_mode(force_json: bool) -> OutputMode {
    if force_json || !std::io::stdout().is_terminal() {
        OutputMode::Json
    } else {
        OutputMode::Human
    }
}

/// Transform raw error messages into AI-friendly guidance with actionable next steps.
/// Only used for Human output mode — JSON mode preserves raw errors.
pub fn format_error(msg: &str, code: Option<&str>) -> String {
    match code {
        Some("ELEMENT_NOT_FOUND") => {
            // Parse "Index N out of range (1-M)" — extract N and M robustly
            let numbers: Vec<&str> = msg
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .collect();
            if numbers.len() >= 3 {
                let idx = numbers[0];
                let max = numbers[2];
                return format!(
                    "Element [{idx}] not found (page has [1]-[{max}]). Re-capture: webpilot capture --dom"
                );
            }
            format!("{msg}. Re-capture: webpilot capture --dom")
        }
        Some("TIMEOUT") => "Timed out. Try: webpilot wait --timeout 15".into(),
        Some("POLICY_DENIED") => "Blocked by policy. Check: webpilot policy list".into(),
        _ if msg.contains("No web page tab") => {
            "No web page open. Navigate: webpilot capture --dom --url URL".into()
        }
        _ if msg.contains("not running") || msg.contains("Not connected") => {
            "Not connected. Run: webpilot install, then reload Chrome extension".into()
        }
        _ if msg.contains("Content Security Policy") || msg.contains("CSP") => {
            "CSP blocks eval. Use: webpilot dom get-text SELECTOR".into()
        }
        _ => msg.to_string(),
    }
}
