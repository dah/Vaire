use super::*;

#[test]
fn provider_error_precedes_malformed_completion_and_usage_siblings() {
    for payload in [
        r#"{"error":{"code":429,"message":"SECRET-REMOTE"},"choices":null,"usage":{"total_tokens":"bad"}}"#,
        r#"{"error":{"code":"429","metadata":{"error_type":"rate_limit_exceeded"}},"choices":[{"index":0,"delta":null}]}"#,
        r#"{"error":{"code":429,"message":"SECRET-CONFLICT","metadata":{"error_type":"authentication"}},"choices":null}"#,
        r#"{"error":{"code":"429","message":"SECRET-CONFLICT","metadata":{"error_type":"authentication"}},"choices":[]}"#,
    ] {
        let error = ChatAccumulator::new().consume(payload).unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
        assert_eq!(error.status(), Some(429));
        assert_eq!(error.stage(), None);
        let debug = format!("{error:?}");
        let display = error.to_string();
        for secret in ["SECRET-REMOTE", "SECRET-CONFLICT"] {
            assert!(!debug.contains(secret));
            assert!(!display.contains(secret));
        }
    }

    let error = ChatAccumulator::new()
        .consume(r#"{"error":[],"choices":[]}"#)
        .unwrap_err();
    assert_eq!(
        error.stage(),
        Some(OpenRouterStreamStage::ProviderErrorShape)
    );
}

#[test]
fn completion_shape_and_invariant_failures_have_exact_stages() {
    for (payload, stage) in [
        ("not-json", OpenRouterStreamStage::ChunkJson),
        ("[]", OpenRouterStreamStage::ChunkJson),
        (r#"{"choices":{}}"#, OpenRouterStreamStage::CompletionShape),
        (
            r#"{"choices":[{},{}]}"#,
            OpenRouterStreamStage::ChoiceCardinality,
        ),
        (
            r#"{"choices":[{"delta":null}]}"#,
            OpenRouterStreamStage::CompletionShape,
        ),
        (
            r#"{"choices":[{"delta":{"content":7}}]}"#,
            OpenRouterStreamStage::CompletionShape,
        ),
        (
            r#"{"choices":[{"delta":{},"finish_reason":{}}]}"#,
            OpenRouterStreamStage::CompletionShape,
        ),
        (
            r#"{"choices":[{"index":1,"delta":{}}]}"#,
            OpenRouterStreamStage::ChoiceIndex,
        ),
        (
            r#"{"id":"","choices":[{"delta":{}}]}"#,
            OpenRouterStreamStage::ResponseId,
        ),
        (
            r#"{"model":"","choices":[{"delta":{}}]}"#,
            OpenRouterStreamStage::Model,
        ),
    ] {
        let error = ChatAccumulator::new().consume(payload).unwrap_err();
        assert_eq!(error.stage(), Some(stage), "payload: {payload}");
    }

    let mut terminal = ChatAccumulator::new();
    terminal
        .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
        .unwrap();
    assert_eq!(
        terminal
            .consume(r#"{"choices":[{"delta":{"content":"late"}}]}"#)
            .unwrap_err()
            .stage(),
        Some(OpenRouterStreamStage::PostTerminal)
    );
    assert!(terminal
        .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
        .unwrap()
        .events
        .is_empty());
    assert_eq!(
        ChatAccumulator::new().finish().unwrap_err().stage(),
        Some(OpenRouterStreamStage::PrematureEof)
    );

    let mut identity = ChatAccumulator::new();
    identity
        .consume(r#"{"id":"first","choices":[{"delta":{"content":"kept"}}]}"#)
        .unwrap();
    assert_eq!(
        identity
            .consume(r#"{"id":"second","choices":[{"delta":{"content":"discarded"}}]}"#)
            .unwrap_err()
            .stage(),
        Some(OpenRouterStreamStage::ResponseId)
    );
    identity
        .consume(r#"{"id":"first","choices":[{"delta":{},"finish_reason":"stop"}]}"#)
        .unwrap();
    assert_eq!(identity.finish().unwrap().0, "kept");

    let mut done = ChatAccumulator::new();
    done.consume("[DONE]").unwrap();
    assert_eq!(
        done.consume(r#"{"choices":[]}"#).unwrap_err().stage(),
        Some(OpenRouterStreamStage::AfterDone)
    );
}
