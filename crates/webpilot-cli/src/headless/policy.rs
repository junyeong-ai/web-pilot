use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

/// File-based policy store (persists across CLI invocations).
fn policy_file() -> std::path::PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::path::Path::new(webpilot::OUTPUT_DIR).join(format!("{user}-policies.json"))
}

fn read_policies() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(policy_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_policies(policies: &std::collections::HashMap<String, String>) {
    let _ = std::fs::write(
        policy_file(),
        serde_json::to_string(policies).unwrap_or_default(),
    );
}

pub(crate) async fn run(args: commands::policy::PolicyArgs, output_mode: OutputMode) -> Result<()> {
    match args.command {
        commands::policy::PolicyCommand::Set { action, verdict } => {
            let mut policies = read_policies();
            policies.insert(action, verdict);
            write_policies(&policies);
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::policy::PolicyCommand::List => {
            let policies = read_policies();
            let list: Vec<_> = policies
                .iter()
                .map(|(k, v)| serde_json::json!({"action_type": k, "verdict": v}))
                .collect();
            match output_mode {
                OutputMode::Human => {
                    for p in &list {
                        eprintln!("{}: {}", p["action_type"], p["verdict"]);
                    }
                    eprintln!("({} rules)", list.len());
                }
                OutputMode::Json => println!("{}", serde_json::json!(list)),
            }
        }
        commands::policy::PolicyCommand::Clear => {
            write_policies(&std::collections::HashMap::new());
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}
