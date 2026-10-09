#![allow(clippy::expect_used)]

use sandbox::network::{InterfaceAddress, NetworkControlMessage};

#[test]
fn interface_address_deserialization_rejects_invalid_prefixes() {
    let error = serde_json::from_str::<InterfaceAddress>(r#"{"address":"192.0.2.2","prefixLength":33}"#)
        .expect_err("IPv4 prefix above 32 should be rejected");

    assert!(error.to_string().contains("prefix length 33 is invalid"));
}

#[test]
fn opaque_control_message_debug_output_does_not_expose_payload() {
    let message = NetworkControlMessage::new(b"must-not-appear");

    let debug = format!("{message:?}");

    assert!(!debug.contains("must-not-appear"));
    assert!(debug.contains("15"));
}
