//! The directly-gated headless-only commands (`context close`, `device`) are
//! denied BEFORE the transport opens: a verdict that is already "no" must not
//! launch Chrome as a side effect. Needs no browser — that is the point: the
//! deny lands first, and `status` (side-effect-free) proves no session was
//! started.

use std::process::Command;

fn webpilot() -> Command {
    Command::new(env!("CARGO_BIN_EXE_webpilot"))
}

#[test]
fn denied_gated_commands_do_not_launch_chrome() {
    let dir = std::env::temp_dir().join(format!("webpilot-pregate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let run = |args: &[&str]| {
        webpilot()
            .args(args)
            .env("WEBPILOT_HOME", &dir)
            .env("WEBPILOT_DATA_HOME", dir.join("data"))
            .output()
            .expect("spawn webpilot")
    };

    assert!(
        run(&["policy", "default", "deny"]).status.success(),
        "policy default deny must apply"
    );

    for args in [&["context", "close", "--all"][..], &["device", "reset"][..]] {
        let out = run(args);
        assert_eq!(
            out.status.code(),
            Some(6),
            "{args:?} under default-deny must be PolicyDenied (6): {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // No session may have been launched by the denied commands: `status` is
    // side-effect-free and must report disconnected with no Chrome started.
    let status = run(&["status"]);
    assert!(status.status.success(), "status must succeed");
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.contains("\"connected\":false") || s.contains("\"connected\": false"),
        "a denied gated command must not have launched Chrome: {s}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
