use super::*;

#[test]
fn portal_tokens_are_monotonic_and_never_collide() {
    let a = next_token("shortcuts_create");
    let b = next_token("shortcuts_create");
    let c = next_token("file_dialog");
    assert_ne!(a, b, "repeated requests for the same prefix must get distinct tokens");
    assert_ne!(b, c);
    assert!(a.starts_with("rune_shortcuts_create_"));
    assert!(c.starts_with("rune_file_dialog_"));
}

#[test]
fn portal_request_path_sanitizes_sender_and_rejects_invalid_inputs() {
    assert_eq!(
        request_path(":1.42", "rune_shortcuts_1").as_deref(),
        Some("/org/freedesktop/portal/desktop/request/1_42/rune_shortcuts_1")
    );
    assert_eq!(
        request_path("1.99", "tok_2").as_deref(),
        Some("/org/freedesktop/portal/desktop/request/1_99/tok_2")
    );
    assert_eq!(request_path("", "tok"), None);
    assert_eq!(request_path(":", "tok"), None);
    assert_eq!(request_path(":1.42", ""), None);
    assert_eq!(request_path(":1.42", "bad-token"), None, "XDG request tokens must be [A-Za-z0-9_]");
    assert_eq!(request_path(":1.42", "bad/token"), None);
}

#[test]
fn settings_portal_watcher_exits_when_portal_is_absent() {
    assert!(should_watch_settings(true, false));
    assert!(should_watch_settings(false, false), "live portal with unset key still watches changes");
    assert!(!should_watch_settings(false, true), "absent portal must not keep watcher thread alive forever");
}

#[test]
fn registry_app_id_trims_and_skips_missing_or_blank_ids() {
    assert_eq!(REGISTRY_INTERFACE, "org.freedesktop.host.portal.Registry");
    assert_eq!(REGISTRY_METHOD, "Register");
    assert_eq!(registry_app_id(None), None);
    assert_eq!(registry_app_id(Some("")), None);
    assert_eq!(registry_app_id(Some("   \t\n")), None);
    assert_eq!(registry_app_id(Some("org.example.App")), Some("org.example.App"));
    assert_eq!(registry_app_id(Some(" org.example.App ")), Some("org.example.App"));
}

#[cfg(all(target_os = "linux", feature = "dbus"))]
#[test]
fn registry_register_body_matches_portal_signature() {
    use std::collections::HashMap;
    use zbus::zvariant::{Type, Value};
    assert_eq!(<(&str, HashMap<&str, Value<'_>>) as Type>::SIGNATURE.to_string(), "(sa{sv})");
}

/// A real error reply arrives as `MethodError`, named; a missing portal or interface is still
/// unavailable then.
#[cfg(all(target_os = "linux", feature = "dbus"))]
#[test]
fn an_error_reply_naming_a_missing_service_is_unavailable() {
    use zbus::message::Message;
    let reply = |name: &str| {
        let call = Message::method_call("/org/freedesktop/portal/desktop", "PreparePrint").unwrap().build(&()).unwrap();
        zbus::Error::from(Message::error(&call.header(), name).unwrap().build(&("detail",)).unwrap())
    };
    assert!(super::is_unavailable(&reply("org.freedesktop.DBus.Error.ServiceUnknown")));
    assert!(super::is_unavailable(&reply("org.freedesktop.DBus.Error.UnknownMethod")));
    assert!(!super::is_unavailable(&reply("org.freedesktop.DBus.Error.AccessDenied")));
    assert!(!super::is_unavailable(&reply("org.freedesktop.portal.Error.NotAllowed")));
}
