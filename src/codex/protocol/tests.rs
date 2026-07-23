use std::path::PathBuf;

use serde_json::json;

use super::{
    classify_message, parse_notification, CancelLoginAccountParams, CancelLoginAccountResponse,
    CancelLoginAccountStatus, InboundMessage, InitializeParams, LoginAccountParams, ProtocolEvent,
    ReasoningSummary, RequestId, ThreadDeleteParams, ThreadDeleteResponse, ThreadItemContent,
    ThreadListParams, ThreadListResponse, ThreadSourceKind, ThreadStartParams,
    MAX_PROTOCOL_IDENTIFIER_BYTES,
};

#[test]
fn initializes_with_exact_vaire_identity_and_without_experimental_capabilities() {
    let params = serde_json::to_value(InitializeParams::vaire()).unwrap();
    assert_eq!(params["clientInfo"]["name"], "vaire");
    assert_eq!(params["clientInfo"]["title"], "Vairë");
    assert_eq!(
        params["clientInfo"]["title"].as_str().unwrap().as_bytes(),
        &[0x56, 0x61, 0x69, 0x72, 0xC3, 0xAB]
    );
    assert_eq!(params["capabilities"]["experimentalApi"], false);
    assert_eq!(
        params["capabilities"]["mcpServerOpenaiFormElicitation"],
        false
    );
    assert_eq!(params["capabilities"]["requestAttestation"], false);
}

#[test]
fn models_login_cancellation_from_the_installed_schema() {
    let params = serde_json::to_value(CancelLoginAccountParams::new("login-active")).unwrap();
    assert_eq!(params, json!({"loginId": "login-active"}));

    let response: CancelLoginAccountResponse =
        serde_json::from_value(json!({"status": "notFound"})).unwrap();
    assert_eq!(response.status, CancelLoginAccountStatus::NotFound);

    let device = serde_json::to_value(LoginAccountParams::chatgpt_device_code()).unwrap();
    assert_eq!(device, json!({"type": "chatgptDeviceCode"}));
}

#[test]
fn models_thread_listing_and_deletion_from_the_installed_schema() {
    let params = serde_json::to_value(ThreadListParams {
        source_kinds: vec![ThreadSourceKind::AppServer, ThreadSourceKind::Vscode],
        archived: false,
        cursor: None,
        cwd: PathBuf::from("/tmp/conversation"),
        limit: 50,
        sort_direction: "desc".to_owned(),
        sort_key: "updated_at".to_owned(),
    })
    .unwrap();
    assert_eq!(params["sourceKinds"], json!(["appServer", "vscode"]));
    assert_eq!(params["cwd"], "/tmp/conversation");

    let start = serde_json::to_value(ThreadStartParams {
        thread_source: ThreadSourceKind::AppServer,
        approval_policy: "never".to_owned(),
        config: json!({}),
        cwd: PathBuf::from("/tmp/conversation"),
        sandbox: "danger-full-access".to_owned(),
        model: "m1".to_owned(),
    })
    .unwrap();
    assert_eq!(start["threadSource"], "appServer");
    let deletion = serde_json::to_value(ThreadDeleteParams {
        thread_id: "thr-old".to_owned(),
    })
    .unwrap();
    assert_eq!(deletion, json!({"threadId": "thr-old"}));
    let _: ThreadDeleteResponse = serde_json::from_value(json!({})).unwrap();
    assert!(serde_json::from_value::<ThreadDeleteResponse>(json!(null)).is_err());
    assert!(serde_json::from_value::<ThreadDeleteResponse>(json!([])).is_err());

    let list: ThreadListResponse = serde_json::from_value(json!({
        "data": [{
            "id": "thr-1",
            "name": null,
            "preview": "hello",
            "createdAt": 10,
            "updatedAt": 20,
            "cwd": "/tmp/conversation",
            "ephemeral": false,
            "source": "appServer"
        }],
        "nextCursor": "page-2"
    }))
    .unwrap();
    assert_eq!(list.data[0].updated_at, 20);
    assert_eq!(list.next_cursor.as_deref(), Some("page-2"));
    assert!(serde_json::from_value::<ThreadListResponse>(json!({
        "data": [{"id": "thr-malformed", "preview": "missing required fields"}]
    }))
    .is_err());
}

#[test]
fn separates_responses_notifications_and_server_requests() {
    assert!(matches!(
        classify_message(json!({"id": 1, "result": {"ok": true}})).unwrap(),
        InboundMessage::Response {
            id: RequestId::Number(1),
            result: Ok(_)
        }
    ));
    assert!(matches!(
        classify_message(json!({"method": "thread/started", "params": {}})).unwrap(),
        InboundMessage::Notification { .. }
    ));
    assert!(matches!(
        classify_message(json!({"id": "s1", "method": "item/tool/call", "params": {}})).unwrap(),
        InboundMessage::ServerRequest { .. }
    ));
}

#[test]
fn rejects_ambiguous_responses() {
    let error = classify_message(json!({"id": 1, "result": {}, "error": {}})).unwrap_err();
    assert!(error.contains("exactly one"));

    for malformed in [
        json!([]),
        json!({"id": -1, "result": {}}),
        json!({"id": 1.5, "result": {}}),
        json!({"id": 1}),
        json!({"id": 1, "error": {"code": "bad", "message": "failure"}}),
        json!({"method": 7, "params": {}}),
        json!({"id": 1, "method": 7, "result": {}}),
        json!({"id": 1, "method": "approval", "result": {}}),
        json!({"method": "notice", "error": {"code": -1, "message": "bad"}}),
    ] {
        assert!(
            classify_message(malformed).is_err(),
            "malformed JSON-RPC frame was classified as usable"
        );
    }
}

#[test]
fn decodes_required_stream_scope_and_tolerates_unknown_notifications() {
    let event = parse_notification(
        "item/agentMessage/delta",
        json!({"threadId":"thr","turnId":"turn","itemId":"item","delta":"hi"}),
    )
    .unwrap();
    assert!(
        matches!(event, Some(ProtocolEvent::AgentMessageDelta(delta)) if delta.item_id == "item")
    );
    assert_eq!(
        parse_notification("future/event", json!({"anything": true})).unwrap(),
        None
    );
    assert!(parse_notification("turn/completed", json!({"threadId":"thr"})).is_err());
}

#[test]
fn rejects_empty_event_scope_and_incomplete_agent_snapshots() {
    for (method, params) in [
        (
            "item/agentMessage/delta",
            json!({"threadId":"", "turnId":"turn", "itemId":"item", "delta":"hi"}),
        ),
        (
            "turn/started",
            json!({
                "threadId":"thr",
                "turn":{"id":"", "items":[], "status":"inProgress"}
            }),
        ),
        (
            "item/completed",
            json!({
                "threadId":"thr", "turnId":"turn",
                "item":{"id":"item", "type":"agentMessage"}
            }),
        ),
        (
            "turn/started",
            json!({
                "threadId":"thr",
                "turn":{"id":"turn", "items":[], "status":"completed"}
            }),
        ),
        (
            "item/agentMessage/delta",
            json!({
                "threadId":"thr", "turnId":"turn", "itemId":"bad\nid", "delta":"x"
            }),
        ),
        (
            "item/agentMessage/delta",
            json!({
                "threadId":"thr", "turnId":"turn",
                "itemId":"x".repeat(MAX_PROTOCOL_IDENTIFIER_BYTES + 1), "delta":"x"
            }),
        ),
    ] {
        assert!(
            parse_notification(method, params).is_err(),
            "{method} accepted an unusable scope or snapshot"
        );
    }
}

#[test]
fn decodes_installed_reasoning_notifications_and_completed_snapshot() {
    let summary = parse_notification(
        "item/reasoning/summaryTextDelta",
        json!({
            "threadId":"thr", "turnId":"turn", "itemId":"why",
            "summaryIndex":1, "delta":"checking"
        }),
    )
    .unwrap();
    assert!(matches!(
        summary,
        Some(ProtocolEvent::ReasoningSummaryTextDelta(delta))
            if delta.summary_index == 1 && delta.delta == "checking"
    ));

    let part = parse_notification(
        "item/reasoning/summaryPartAdded",
        json!({
            "threadId":"thr", "turnId":"turn", "itemId":"why", "summaryIndex":2
        }),
    )
    .unwrap();
    assert!(matches!(
        part,
        Some(ProtocolEvent::ReasoningSummaryPartAdded(part)) if part.summary_index == 2
    ));

    let text = parse_notification(
        "item/reasoning/textDelta",
        json!({
            "threadId":"thr", "turnId":"turn", "itemId":"why",
            "contentIndex":0, "delta":"emitted"
        }),
    )
    .unwrap();
    assert!(matches!(
        text,
        Some(ProtocolEvent::ReasoningTextDelta(delta))
            if delta.content_index == 0 && delta.delta == "emitted"
    ));

    let completed = parse_notification(
        "item/completed",
        json!({
            "threadId":"thr", "turnId":"turn", "completedAtMs":1,
            "item": {
                "id":"why", "type":"reasoning",
                "summary":["checking facts"], "content":["emitted detail"]
            }
        }),
    )
    .unwrap();
    assert!(matches!(
        completed,
        Some(ProtocolEvent::ItemCompleted(completed))
            if completed.item.summary == ["checking facts"]
                && matches!(completed.item.content.as_slice(), [ThreadItemContent::Text(value)] if value == "emitted detail")
    ));

    assert!(parse_notification(
        "item/reasoning/summaryTextDelta",
        json!({"threadId":"thr", "turnId":"turn", "itemId":"why", "delta":"missing index"}),
    )
    .is_err());
    assert_eq!(
        serde_json::to_value(ReasoningSummary::Auto).unwrap(),
        json!("auto")
    );
    assert_eq!(
        serde_json::to_value(ReasoningSummary::Detailed).unwrap(),
        json!("detailed")
    );
}

#[test]
fn decodes_token_usage_from_the_installed_schema() {
    let event = parse_notification(
        "thread/tokenUsage/updated",
        json!({
            "threadId": "thr",
            "turnId": "turn",
            "tokenUsage": {
                "last": {
                    "cachedInputTokens": 0,
                    "inputTokens": 10,
                    "outputTokens": 5,
                    "reasoningOutputTokens": 2,
                    "totalTokens": 17
                },
                "total": {
                    "cachedInputTokens": 0,
                    "inputTokens": 30,
                    "outputTokens": 10,
                    "reasoningOutputTokens": 5,
                    "totalTokens": 45
                },
                "modelContextWindow": 100
            }
        }),
    )
    .unwrap();

    assert!(matches!(
        event,
        Some(ProtocolEvent::ThreadTokenUsageUpdated(usage))
            if usage.thread_id == "thr"
                && usage.turn_id == "turn"
                && usage.token_usage.last.total_tokens == 17
                && usage.token_usage.total.total_tokens == 45
                && usage.token_usage.model_context_window == Some(100)
    ));

    let null_window = parse_notification(
        "thread/tokenUsage/updated",
        json!({
            "threadId": "thr",
            "turnId": "turn",
            "tokenUsage": {
                "last": { "totalTokens": 5 },
                "total": { "totalTokens": 45 },
                "modelContextWindow": null
            }
        }),
    )
    .unwrap();
    assert!(matches!(
        null_window,
        Some(ProtocolEvent::ThreadTokenUsageUpdated(usage))
            if usage.token_usage.model_context_window.is_none()
    ));
}

#[test]
fn rejects_malformed_and_out_of_range_token_usage() {
    for malformed in [
        json!({"threadId":"thr","turnId":"turn"}),
        json!({"threadId":"thr","turnId":"turn","tokenUsage":{}}),
        json!({
            "threadId":"thr","turnId":"turn",
            "tokenUsage":{"last":{"totalTokens":5}}
        }),
        json!({
            "threadId":"thr","turnId":"turn",
            "tokenUsage":{"total":{"totalTokens":45}}
        }),
        json!({
            "threadId":"thr","turnId":"turn",
            "tokenUsage":{"last":{},"total":{"totalTokens":45}}
        }),
        json!({
            "threadId":"thr","turnId":"turn",
            "tokenUsage":{
                "last":{"totalTokens":"5"},
                "total":{"totalTokens":45}
            }
        }),
    ] {
        assert!(parse_notification("thread/tokenUsage/updated", malformed).is_err());
    }

    let too_large = serde_json::from_str(
            r#"{"threadId":"thr","turnId":"turn","tokenUsage":{"last":{"totalTokens":9223372036854775808},"total":{"totalTokens":45},"modelContextWindow":100}}"#,
        )
        .unwrap();
    assert!(parse_notification("thread/tokenUsage/updated", too_large).is_err());
}
