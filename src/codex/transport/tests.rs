use std::time::Duration;

use serde_json::json;

use super::{
    bounded_json_value, encode_frame, OutboundRequestIds, RequestTimeouts, RetiredRequestIds,
    TransportError, MAX_FRAME_BYTES, RETIRED_REQUEST_CAPACITY,
};

#[test]
fn retired_request_correlation_is_bounded_and_keeps_the_newest_ids() {
    let mut retired = RetiredRequestIds::default();
    for id in 1..=(RETIRED_REQUEST_CAPACITY as u64 + 1) {
        retired.insert(id);
    }

    assert_eq!(retired.ids.len(), RETIRED_REQUEST_CAPACITY);
    assert!(!retired.remove(1));
    assert!(retired.remove(RETIRED_REQUEST_CAPACITY as u64 + 1));
}

#[test]
fn login_cancellation_uses_the_authentication_timeout() {
    let timeouts = RequestTimeouts {
        auth: Duration::from_secs(37),
        fallback: Duration::from_secs(3),
        ..RequestTimeouts::default()
    };
    assert_eq!(
        timeouts.for_method("account/login/cancel"),
        Duration::from_secs(37)
    );
}

#[test]
fn outbound_request_ids_never_saturate_into_duplicates() {
    let mut ids = OutboundRequestIds {
        next: Some(u64::MAX),
    };
    assert_eq!(ids.allocate().unwrap(), u64::MAX);
    assert!(ids.allocate().is_err());
}

#[test]
fn outbound_serialization_is_bounded_and_frame_limit_excludes_the_delimiter() {
    let template = json!({"method":"exact","params":{"value":""}});
    let overhead = serde_json::to_vec(&template).unwrap().len();
    let exact = json!({
        "method":"exact",
        "params":{"value":"x".repeat(MAX_FRAME_BYTES - overhead)}
    });
    let encoded = encode_frame(&exact).unwrap();
    assert_eq!(encoded.len(), MAX_FRAME_BYTES + 1);
    assert_eq!(encoded.last(), Some(&b'\n'));

    let oversized = json!({
        "method":"exact",
        "params":{"value":"x".repeat(MAX_FRAME_BYTES - overhead + 1)}
    });
    assert!(matches!(
        encode_frame(&oversized),
        Err(TransportError::Protocol(message)) if message.contains("size limit")
    ));
    let oversized_method = json!({
        "method":"x".repeat(MAX_FRAME_BYTES + 1),
        "params":{}
    });
    assert!(matches!(
        encode_frame(&oversized_method),
        Err(TransportError::Protocol(message)) if message.contains("size limit")
    ));
    assert!(matches!(
        bounded_json_value(json!({"value":"x".repeat(MAX_FRAME_BYTES)})),
        Err(TransportError::Protocol(message)) if message.contains("size limit")
    ));
}
