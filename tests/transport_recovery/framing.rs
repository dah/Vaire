use super::support::*;

#[tokio::test]
async fn malformed_json_is_reported_as_a_framing_failure() {
    let temp = tempdir().unwrap();
    let executable = script(temp.path(), "printf '%s\\n' '{broken'\nsleep 1");
    let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), transport.next_event())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        event.event,
        InboundEvent::ConnectionClosed { category } if category == "framing"
    ));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn codec_rejects_oversized_and_invalid_utf8_lines_without_poisoning_later_connections() {
    let cases = [
        (
            "oversized-line",
            format!(
                "dd if=/dev/zero bs={} count=1 2>/dev/null | tr '\\000' x\nprintf '\\n'\nIFS= read -r hold || true",
                MAX_FRAME_BYTES + 1
            ),
        ),
        (
            "invalid-utf8",
            "printf '\\377\\n'\nIFS= read -r hold || true".to_owned(),
        ),
    ];

    for (name, body) in cases {
        let temp = tempdir().unwrap();
        let executable = script(temp.path(), &body);
        let mut transport = AppServerTransport::spawn(spec(temp.path(), executable))
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), transport.next_event())
            .await
            .unwrap_or_else(|_| panic!("{name} did not close the connection"))
            .expect("framing failure event was missing");
        assert!(
            matches!(event.event, InboundEvent::ConnectionClosed { category } if category == "framing"),
            "{name} was not classified as a codec framing failure"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), transport.next_event())
                .await
                .expect("connection worker did not finish")
                .is_none(),
            "{name} emitted data after its terminal framing event"
        );
        assert_eq!(
            transport
                .request("after-framing-failure", json!({}), Duration::from_secs(1))
                .await,
            Err(TransportError::Closed),
            "{name} left a partially usable transport behind"
        );
        transport.shutdown().await.unwrap();
    }
}
