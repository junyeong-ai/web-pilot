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

#[test]
fn explicit_config_path_that_does_not_exist_fails_loud() {
    // The DEFAULT config path being absent is the all-default state — but a
    // path the operator set EXPLICITLY via WEBPILOT_CONFIG and got wrong must
    // not silently run on built-in defaults, ignoring every setting they
    // intended to apply.
    let dir = std::env::temp_dir().join(format!("webpilot-cfgmiss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = webpilot()
        .args(["capture", "--include", "dom"])
        .env("WEBPILOT_CONFIG", dir.join("does-not-exist.toml"))
        .env("WEBPILOT_HOME", &dir)
        .output()
        .expect("spawn webpilot");
    assert_eq!(
        out.status.code(),
        Some(7),
        "an explicitly-set but missing WEBPILOT_CONFIG must be a typed \
         InvalidArgument, not a silent fall-back to defaults; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Piped stdout auto-selects JSON mode, so the typed error body lands there.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("does not exist"),
        "the error must name the missing path: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_viewport_dimensions_are_refused_at_startup() {
    // `--window-size=0,0` / a 0×0 emulation override degrade the session
    // instead of failing — the validator must refuse them up front, like the
    // other zero-breaks-downstream values.
    let dir = std::env::temp_dir().join(format!("webpilot-cfgvp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    for var in ["WEBPILOT_VIEWPORT_WIDTH", "WEBPILOT_VIEWPORT_HEIGHT"] {
        let out = webpilot()
            .args(["status"])
            .env(var, "0")
            .env("WEBPILOT_HOME", &dir)
            .output()
            .expect("spawn webpilot");
        assert_eq!(
            out.status.code(),
            Some(7),
            "a zero {var} must be refused at startup; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_webpilot_config_means_unset_and_reads_the_default_path() {
    // An EMPTY WEBPILOT_CONFIG is unset — the same rule `dirs::env_path`
    // resolves the path with — so the DEFAULT path must be read (proven by a
    // malformed default config failing loud with the parse error), never the
    // "points at a path that does not exist" override error.
    let dir = std::env::temp_dir().join(format!("webpilot-cfgempty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("config.toml"), "[timeouts\n").expect("write cfg");
    let out = webpilot()
        .args(["capture", "--include", "dom"])
        .env("WEBPILOT_CONFIG", "")
        .env("WEBPILOT_HOME", &dir)
        .output()
        .expect("spawn webpilot");
    assert_eq!(
        out.status.code(),
        Some(7),
        "the malformed DEFAULT config must gate the run; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        body.contains("invalid settings") && !body.contains("does not exist"),
        "an empty WEBPILOT_CONFIG must resolve the DEFAULT path (parse error), \
         not error as a missing override: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
