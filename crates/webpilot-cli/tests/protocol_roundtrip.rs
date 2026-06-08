//! End-to-end protocol round-trip: every wire shape we ship must serialize
//! and deserialize without loss. These tests guard the CLI parser, the IPC
//! envelope, and the bridge.js error contract from drifting apart.

use webpilot::action::{Action, Modifiers, ScrollDir};
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::error::{WebPilotError, WireError};
use webpilot::protocol::{Command, DomProperty, FrameSelector, Request, Response, ResponseData};
use webpilot::types::{ConsoleLevel, PolicyVerdict};
use webpilot::wait::WaitCondition;

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_value(value).expect("serialize");
    serde_json::from_value(json).expect("deserialize")
}

#[test]
fn action_click_round_trips() {
    let a = Action::Click { index: 7 };
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["kind"], "click");
    assert_eq!(v["index"], 7);
    let back: Action = serde_json::from_value(v).unwrap();
    assert!(matches!(back, Action::Click { index: 7 }));
}

#[test]
fn action_scroll_uses_typed_direction() {
    let a = Action::Scroll {
        direction: ScrollDir::Up,
        amount: 400,
    };
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["kind"], "scroll");
    assert_eq!(v["direction"], "up");
    assert_eq!(v["amount"], 400);
}

#[test]
fn action_key_press_carries_modifiers() {
    let a = Action::KeyPress {
        key: "Enter".into(),
        modifiers: Modifiers {
            ctrl: true,
            ..Default::default()
        },
    };
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["kind"], "key_press");
    assert_eq!(v["modifiers"]["ctrl"], true);
    assert_eq!(v["modifiers"]["shift"], false);
}

#[test]
fn capture_command_uses_include_list() {
    let cmd = Command::Capture {
        include: vec![CaptureField::Dom, CaptureField::Screenshot],
        opts: CaptureOpts {
            bounds: true,
            ..Default::default()
        },
        url: Some("https://example.com".into()),
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "Capture");
    assert_eq!(v["include"][0], "dom");
    assert_eq!(v["include"][1], "screenshot");
    assert_eq!(v["opts"]["bounds"], true);
}

#[test]
fn wait_condition_is_tagged() {
    let cmd = Command::Wait {
        condition: WaitCondition::Selector {
            value: ".loading".into(),
        },
        timeout_ms: 5_000,
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "Wait");
    assert_eq!(v["condition"]["until"], "selector");
    assert_eq!(v["condition"]["value"], ".loading");
}

#[test]
fn frame_selector_uses_by_tag() {
    let cmd = Command::FrameSwitch {
        selector: FrameSelector::Url {
            pattern: "/auth/".into(),
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "FrameSwitch");
    assert_eq!(v["selector"]["by"], "url");
    assert_eq!(v["selector"]["pattern"], "/auth/");
    let back: Command = serde_json::from_value(v).unwrap();
    assert!(matches!(
        back,
        Command::FrameSwitch {
            selector: FrameSelector::Url { .. }
        }
    ));
}

#[test]
fn dom_property_attr_carries_name() {
    let cmd = Command::DomGet {
        selector: "#x".into(),
        property: DomProperty::Attr {
            name: "href".into(),
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "DomGet");
    assert_eq!(v["property"]["kind"], "attr");
    assert_eq!(v["property"]["name"], "href");
}

#[test]
fn request_envelope_serializes_id_and_command() {
    let req = Request::new(42, Command::Eval { code: "1+1".into() });
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["id"], 42);
    assert_eq!(v["command"]["type"], "Eval");
    assert_eq!(v["command"]["code"], "1+1");
}

#[test]
fn command_policy_key_gates_by_effect() {
    use webpilot::Action;
    use webpilot::protocol::FrameSelector;
    use webpilot::types::PolicyKey;

    // Gated.
    assert_eq!(
        Command::Action {
            action: Action::Click { index: 1 },
            capture: false,
        }
        .policy_key(),
        Some(PolicyKey::Click)
    );
    assert_eq!(
        Command::Eval { code: "x".into() }.policy_key(),
        Some(PolicyKey::Eval)
    );
    assert_eq!(
        Command::SessionExport.policy_key(),
        Some(PolicyKey::SessionExport)
    );
    assert_eq!(
        Command::FrameSwitch {
            selector: FrameSelector::Predicate { js: "true".into() },
        }
        .policy_key(),
        Some(PolicyKey::Eval)
    );

    // Keyed by effect, not command name: every navigation surface gates under
    // `navigate`, and cookie-value reads under `cookie_list`.
    assert_eq!(
        Command::Capture {
            include: vec![],
            opts: Default::default(),
            url: Some("http://x".into()),
        }
        .policy_key(),
        Some(PolicyKey::Navigate),
        "capture --url navigates, so it must gate under navigate"
    );
    assert_eq!(
        Command::TabNew {
            url: "http://x".into()
        }
        .policy_key(),
        Some(PolicyKey::Navigate),
        "tab new URL loads a URL, so it must gate under navigate"
    );
    assert_eq!(
        Command::CookieList { url: "u".into() }.policy_key(),
        Some(PolicyKey::CookieList),
        "cookie list/get returns credential values, so it is gated"
    );
    assert_eq!(
        Command::TabClose { tab_id: "1".into() }.policy_key(),
        Some(PolicyKey::TabClose),
        "closing a tab is a destructive browser-state mutation"
    );
    // Monitoring hooks inject MAIN-world JS → gated as eval; their buffer
    // read/clear is bookkeeping and stays ungated.
    assert_eq!(Command::ConsoleStart.policy_key(), Some(PolicyKey::Eval));
    assert_eq!(Command::NetworkStart.policy_key(), Some(PolicyKey::Eval));

    // Ungated: read-only observation, buffer bookkeeping, structural selection.
    assert_eq!(Command::Status.policy_key(), None);
    assert_eq!(Command::TabList.policy_key(), None);
    assert_eq!(
        Command::Wait {
            condition: WaitCondition::Idle,
            timeout_ms: 1000,
        }
        .policy_key(),
        None
    );
    assert_eq!(Command::ConsoleRead { since: None }.policy_key(), None);
    assert_eq!(Command::ConsoleClear.policy_key(), None);
    assert_eq!(Command::NetworkRead { since: None }.policy_key(), None);
    assert_eq!(Command::NetworkClear.policy_key(), None);
    assert_eq!(Command::FrameList.policy_key(), None);
    assert_eq!(Command::TabSwitch { tab_id: "1".into() }.policy_key(), None);
    assert_eq!(
        Command::DomGet {
            selector: "#x".into(),
            property: DomProperty::Text,
        }
        .policy_key(),
        None,
        "dom get reads rendered DOM, not a credential store"
    );
    assert_eq!(
        Command::FrameSwitch {
            selector: FrameSelector::Name { value: "f".into() },
        }
        .policy_key(),
        None,
        "structural (non-predicate) frame selection runs no caller JS"
    );
    assert_eq!(
        Command::Capture {
            include: vec![],
            opts: Default::default(),
            url: None,
        }
        .policy_key(),
        None,
        "a URL-less capture only reads the current page"
    );
    assert_eq!(
        Command::FrameSwitch {
            selector: FrameSelector::Main,
        }
        .policy_key(),
        None
    );
}

/// Security invariant the NM host's policy enforcement depends on: a gated
/// command with a type-mismatched field must FAIL to deserialize into `Command`.
/// The host rejects anything it cannot parse, so this is what stops a
/// `{"index": "1"}` (string) payload — which the loose JS bridge would coerce
/// and execute — from slipping past a deny rule. If `Action.index` ever gained
/// string coercion, this test fails and flags the reopened bypass.
#[test]
fn type_mismatched_gated_command_is_rejected_by_deserialization() {
    // Well-formed: parses, and is a gated click.
    let ok = serde_json::json!({"type": "Action", "action": {"kind": "click", "index": 1}});
    let parsed: Command = serde_json::from_value(ok).expect("valid click parses");
    assert!(parsed.policy_key().is_some());

    // Index as a string: Rust `u32` rejects it, so the host rejects the payload
    // instead of forwarding it unenforced.
    let bad = serde_json::json!({"type": "Action", "action": {"kind": "click", "index": "1"}});
    assert!(
        serde_json::from_value::<Command>(bad).is_err(),
        "string index must not deserialize — else it bypasses policy host-side"
    );

    // Unknown extra fields do NOT bypass: the command still parses (serde
    // ignores them) and is therefore still policy-checked.
    let extra = serde_json::json!({"type": "Eval", "code": "x", "evil": true});
    let parsed: Command = serde_json::from_value(extra).expect("unknown fields are ignored");
    assert_eq!(parsed.policy_key(), Some(webpilot::types::PolicyKey::Eval));
}

#[test]
fn response_action_carries_typed_error() {
    let r = Response {
        id: 1,
        result: ResponseData::Action {
            success: false,
            error: Some(WebPilotError::ElementNotFound {
                requested: 5,
                available: 3,
            }),
            dom: None,
            url_changed: None,
            new_tab: None,
            capture_error: None,
        },
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["result"]["error"]["code"], "ElementNotFound");
    assert_eq!(v["result"]["error"]["requested"], 5);
    assert_eq!(v["result"]["error"]["available"], 3);

    let back = round_trip(&r);
    if let ResponseData::Action {
        error:
            Some(WebPilotError::ElementNotFound {
                requested,
                available,
            }),
        ..
    } = back.result
    {
        assert_eq!(requested, 5);
        assert_eq!(available, 3);
    } else {
        panic!("expected ElementNotFound, got {:?}", back.result);
    }
}

#[test]
fn wire_error_unknown_code_falls_back_to_other() {
    let bridge_response = serde_json::json!({
        "code": "TotallyMadeUp",
        "message": "ouch",
        "extra": "ignored",
    });
    let wire: WireError = serde_json::from_value(bridge_response).unwrap();
    let typed = WebPilotError::from_wire(wire);
    assert!(matches!(typed, WebPilotError::Other { .. }));
    assert_eq!(typed.to_string(), "ouch");
    assert_eq!(typed.exit_code(), 1);
}

#[test]
fn console_level_round_trips_via_string() {
    for lvl in [
        ConsoleLevel::Log,
        ConsoleLevel::Error,
        ConsoleLevel::Warn,
        ConsoleLevel::Info,
        ConsoleLevel::Debug,
    ] {
        let s = lvl.to_string();
        let back: ConsoleLevel = s.parse().unwrap();
        assert_eq!(back, lvl);
    }
}

#[test]
fn policy_verdict_round_trips_via_string() {
    let allow: PolicyVerdict = "allow".parse().unwrap();
    assert_eq!(allow, PolicyVerdict::Allow);
    let deny: PolicyVerdict = "deny".parse().unwrap();
    assert_eq!(deny, PolicyVerdict::Deny);
    assert!("invalid".parse::<PolicyVerdict>().is_err());
}

/// Action's `kind` JSON discriminator and `ActionKind` (used for policy
/// lookups) MUST emit the identical string per variant. If they ever drift
/// the policy-enforcement check `policies[action.kind]` silently breaks.
#[test]
fn action_kind_matches_action_wire_tag() {
    use webpilot::ActionKind;
    use webpilot::Modifiers;

    let cases: Vec<(Action, ActionKind, &str)> = vec![
        (Action::Click { index: 1 }, ActionKind::Click, "click"),
        (
            Action::Type {
                index: 1,
                text: "x".into(),
                clear: false,
            },
            ActionKind::Type,
            "type",
        ),
        (
            Action::KeyPress {
                key: "Enter".into(),
                modifiers: Modifiers::default(),
            },
            ActionKind::KeyPress,
            "key_press",
        ),
        (
            Action::Navigate { url: "/".into() },
            ActionKind::Navigate,
            "navigate",
        ),
        (
            Action::Scroll {
                direction: webpilot::ScrollDir::Down,
                amount: 100,
            },
            ActionKind::Scroll,
            "scroll",
        ),
        (
            Action::ScrollTo { index: 2 },
            ActionKind::ScrollTo,
            "scroll_to",
        ),
        (Action::Back, ActionKind::Back, "back"),
        (Action::Forward, ActionKind::Forward, "forward"),
        (Action::Reload, ActionKind::Reload, "reload"),
        (Action::Hover { index: 1 }, ActionKind::Hover, "hover"),
        (Action::Focus { index: 1 }, ActionKind::Focus, "focus"),
        (
            Action::Select {
                index: 1,
                value: "v".into(),
            },
            ActionKind::Select,
            "select",
        ),
        (
            Action::Upload {
                index: 1,
                path: "/tmp/x".into(),
            },
            ActionKind::Upload,
            "upload",
        ),
        (
            Action::Drag {
                source: 1,
                target: 2,
                steps: 5,
            },
            ActionKind::Drag,
            "drag",
        ),
    ];
    for (action, kind, expected) in cases {
        // 1. Action wire `kind` field
        let action_json = serde_json::to_value(&action).unwrap();
        assert_eq!(
            action_json["kind"], expected,
            "Action::{kind:?} wire kind field"
        );
        // 2. Action::kind() returns matching enum variant
        assert_eq!(action.kind(), kind, "Action::{kind:?}.kind()");
        // 3. ActionKind serde matches
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            expected,
            "ActionKind::{kind:?} serde"
        );
        // 4. Display + FromStr round-trip uses same string
        assert_eq!(kind.to_string(), expected, "ActionKind::{kind:?} Display");
        let parsed: ActionKind = expected.parse().unwrap();
        assert_eq!(parsed, kind, "ActionKind FromStr({expected})");
    }
}
