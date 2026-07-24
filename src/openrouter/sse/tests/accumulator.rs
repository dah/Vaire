use super::*;

#[test]
fn accumulator_accepts_finish_then_usage_and_eof() {
    let mut state = ChatAccumulator::new();
    let first = state
        .consume(
            r#"{"id":"x","model":"vendor/model","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    assert_eq!(
        first.events,
        vec![ChatStreamEvent::TextDelta("ok".to_owned())]
    );
    let usage = state
        .consume(r#"{"id":"x","model":"vendor/model","choices":[],"usage":{"total_tokens":3}}"#)
        .unwrap();
    assert_eq!(
        usage.events,
        vec![ChatStreamEvent::Usage(TokenUsage {
            total_tokens: 3,
            ..TokenUsage::default()
        })]
    );
    assert_eq!(
        state.finish().unwrap(),
        (
            "ok".to_owned(),
            Some(TokenUsage {
                total_tokens: 3,
                ..TokenUsage::default()
            })
        )
    );
}

#[test]
fn semantic_model_is_established_by_the_server_not_the_request() {
    let mut state = ChatAccumulator::new();
    state
        .consume(r#"{"id":"x","model":"vendor/resolved-v2","choices":[{"index":0,"delta":{"content":"ok"}}]}"#)
        .unwrap();
    state
        .consume(
            r#"{"id":"metadata-only","model":"vendor/other-metadata","choices":[],"usage":null}"#,
        )
        .unwrap();
    state
        .consume(r#"{"id":"x","model":"vendor/resolved-v2","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)
        .unwrap();
    assert_eq!(state.finish().unwrap().0, "ok");

    let mut conflicting = ChatAccumulator::new();
    conflicting
        .consume(r#"{"model":"vendor/resolved-v1","choices":[{"delta":{"content":"kept"}}]}"#)
        .unwrap();
    let error = conflicting
        .consume(r#"{"model":"vendor/resolved-v2","choices":[{"delta":{"content":"discarded"}}]}"#)
        .unwrap_err();
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::Model));
    conflicting
        .consume(
            r#"{"model":"vendor/resolved-v1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    assert_eq!(conflicting.finish().unwrap().0, "kept");
}

#[test]
fn metadata_choices_and_usage_are_independently_tolerant() {
    let mut state = ChatAccumulator::new();
    for payload in [
        r#"{"id":"ignored-a","model":"ignored-a"}"#,
        r#"{"id":null,"model":null,"choices":null,"usage":null}"#,
        r#"{"id":"","model":"","choices":[],"usage":{"prompt_tokens":2}}"#,
    ] {
        state.consume(payload).unwrap();
    }
    let (_, usage) = state
        .consume("[DONE]")
        .and_then(|_| state.finish())
        .unwrap();
    assert_eq!(usage.unwrap().prompt_tokens, 2);
}

#[test]
fn malformed_usage_is_dropped_atomically_once_and_preserves_prior_usage() {
    let mut state = ChatAccumulator::new();
    state
        .consume(r#"{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#)
        .unwrap();
    for (index, usage) in [
        r#"{"total_tokens":null}"#,
        r#"{"total_tokens":"3"}"#,
        r#"{"total_tokens":-1}"#,
        r#"{"total_tokens":1.5}"#,
        r#"{"total_tokens":18446744073709551616}"#,
        r#"[]"#,
    ]
    .into_iter()
    .enumerate()
    {
        let result = state
            .consume(&format!(r#"{{"choices":[],"usage":{usage}}}"#))
            .unwrap();
        assert!(result.events.is_empty());
        assert_eq!(
            result.compatibility_stage,
            (index == 0).then_some(OpenRouterStreamStage::UsageDropped)
        );
    }
    let (answer, usage) = state.finish().unwrap();
    assert_eq!(answer, "answer");
    assert_eq!(usage.unwrap().total_tokens, 3);
}

#[test]
fn repeated_empty_non_error_finish_markers_are_idempotent_and_strict() {
    for repeated_reason in ["stop", "length"] {
        let mut state = ChatAccumulator::new();
        state
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        let duplicate = state
            .consume(&format!(
                r#"{{"id":"x","model":"m","choices":[{{"index":0,"delta":{{}},"finish_reason":"{repeated_reason}"}}],"usage":{{"total_tokens":3}}}}"#
            ))
            .unwrap();
        assert_eq!(
            duplicate.events,
            vec![ChatStreamEvent::Usage(TokenUsage {
                total_tokens: 3,
                ..TokenUsage::default()
            })]
        );
        state.consume("[DONE]").unwrap();
        assert_eq!(
            state.finish().unwrap(),
            (
                "ok".to_owned(),
                Some(TokenUsage {
                    total_tokens: 3,
                    ..TokenUsage::default()
                })
            )
        );
    }

    for (payload, stage) in [
        (
            r#"{"id":"other","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            OpenRouterStreamStage::ResponseId,
        ),
        (
            r#"{"id":"x","model":"other","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            OpenRouterStreamStage::Model,
        ),
        (
            r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"late"},"finish_reason":"length"}]}"#,
            OpenRouterStreamStage::PostTerminal,
        ),
    ] {
        let mut state = ChatAccumulator::new();
        state
            .consume(
                r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assert_eq!(state.consume(payload).unwrap_err().stage(), Some(stage));
    }

    let mut bare_error = ChatAccumulator::new();
    bare_error
        .consume(
            r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    let error = bare_error
        .consume(
            r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"error"}]}"#,
        )
        .unwrap_err();
    assert_eq!(error.category(), OpenRouterFailureCategory::Remote);
    assert_eq!(error.stage(), None);

    for payload in [
        r#"{"error":{"code":429},"choices":[{"index":0,"delta":{"content":"late"}}]}"#,
        r#"{"error":{"code":"429"},"choices":[{"index":0,"delta":null}]}"#,
    ] {
        let mut state = ChatAccumulator::new();
        state
            .consume(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        let error = state.consume(payload).unwrap_err();
        assert_eq!(error.category(), OpenRouterFailureCategory::RateLimited);
        assert_eq!(error.status(), Some(429));
        assert_eq!(error.stage(), None);
    }

    let mut malformed_usage = ChatAccumulator::new();
    malformed_usage
        .consume(
            r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    let duplicate = malformed_usage
        .consume(
            r#"{"id":"x","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"length"}],"usage":{"total_tokens":"bad"}}"#,
        )
        .unwrap();
    assert!(duplicate.events.is_empty());
    assert_eq!(
        duplicate.compatibility_stage,
        Some(OpenRouterStreamStage::UsageDropped)
    );
    assert_eq!(malformed_usage.finish().unwrap(), ("ok".to_owned(), None));
}

#[test]
fn reasoning_only_deltas_are_semantic_and_assistant_limit_is_staged() {
    let mut state = ChatAccumulator::new();
    assert!(state
        .consume(r#"{"model":"resolved","choices":[{"delta":{"reasoning":"private"}}]}"#)
        .unwrap()
        .events
        .is_empty());
    state
        .consume(
            r#"{"model":"resolved","choices":[{"delta":{"content":null},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    assert_eq!(state.finish().unwrap().0, "");

    let content = "x".repeat(MAX_ASSISTANT_BYTES + 1);
    let payload = serde_json::json!({
        "choices": [{"delta": {"content": content}}]
    })
    .to_string();
    let error = ChatAccumulator::new().consume(&payload).unwrap_err();
    assert_eq!(error.category(), OpenRouterFailureCategory::ResourceLimit);
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::AssistantLimit));
}
