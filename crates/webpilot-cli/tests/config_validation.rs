//! `config.toml` validation gates only the commands that read settings.
//!
//! A malformed file fails the settings-backed paths (transport / headless /
//! browser / MCP) loudly, but the pure-local commands (`policy`, `setup`,
//! `uninstall`, `diff`, `self`) read no settings and must stay usable to
//! inspect or repair an install even when `config.toml` is itself what is
//! broken — a security `policy default deny` or a recovery `setup` /
//! `uninstall` must not be held hostage by an unrelated `[timeouts]` typo.
//!
//! Needs no browser: the settings-backed path fails at validation, before any
//! Chrome launch, so this runs in the normal `cargo test` pass.

use std::process::Command;

fn webpilot() -> Command {
    Command::new(env!("CARGO_BIN_EXE_webpilot"))
}

#[test]
fn malformed_config_gates_settings_paths_but_not_local_commands() {
    let dir = std::env::temp_dir().join(format!("webpilot-cfgtest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let cfg = dir.join("config.toml");
    // An unclosed table header — `toml::from_str` rejects it, which
    // `settings::init` surfaces as a typed InvalidArgument.
    std::fs::write(&cfg, "[timeouts\nnavigation_ms = 5000\n").expect("write cfg");

    let run = |args: &[&str]| {
        webpilot()
            .args(args)
            .env("WEBPILOT_CONFIG", &cfg)
            .env("WEBPILOT_HOME", &dir)
            .output()
            .expect("spawn webpilot")
    };

    // A pure-local command reads no settings → it runs despite the broken
    // config (an absent policy store is the permissive default, so `list`
    // succeeds and prints it).
    let local = run(&["policy", "list"]);
    assert!(
        local.status.success(),
        "`policy list` must work with a malformed config.toml — it reads no \
         settings; got exit {:?}, stderr: {}",
        local.status.code(),
        String::from_utf8_lossy(&local.stderr)
    );

    // A settings-backed path is still gated loudly: InvalidArgument (exit 7),
    // raised at validation BEFORE any Chrome launch (so this assertion needs no
    // browser and cannot hang).
    let transport = run(&["capture", "--include", "dom"]);
    assert_eq!(
        transport.status.code(),
        Some(7),
        "a settings-backed command must fail with InvalidArgument (7) on a \
         malformed config, before opening Chrome; stderr: {}",
        String::from_utf8_lossy(&transport.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
