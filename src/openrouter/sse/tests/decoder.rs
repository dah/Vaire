use super::*;

#[test]
fn decoder_handles_fragmentation_crlf_and_multiline_data() {
    let mut decoder = SseDecoder::new();
    assert!(decoder.push(b"da").unwrap().is_empty());
    assert!(decoder
        .push(b"ta: {\"a\":\r\ndata: 1}\r\n\r\n")
        .unwrap()
        .contains(&"{\"a\":\n1}".to_owned()));
}

#[test]
fn decoder_accepts_a_large_transport_chunk_of_small_events() {
    let event = b"data: {}\n\n";
    let count = MAX_SSE_EVENT_BYTES / event.len() + 100;
    let chunk = event.repeat(count);
    assert!(chunk.len() > MAX_SSE_EVENT_BYTES);
    let mut decoder = SseDecoder::new();
    let events = decoder.push(&chunk).unwrap();
    assert_eq!(events.len(), count);
    assert!(events.iter().all(|event| event == "{}"));
}

#[test]
fn decoder_rejects_an_oversize_line_without_unbounded_growth() {
    let mut decoder = SseDecoder::new();
    let oversized = vec![b'x'; MAX_SSE_EVENT_BYTES + 1];
    let error = decoder.push(&oversized).unwrap_err();
    assert_eq!(error.category(), OpenRouterFailureCategory::ResourceLimit);
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::SseFrameLimit));
}

#[test]
fn decoder_rejects_invalid_data_utf8_with_a_static_stage() {
    let mut decoder = SseDecoder::new();
    let error = decoder.push(b"data: \xff\n\n").unwrap_err();
    assert_eq!(error.category(), OpenRouterFailureCategory::InvalidResponse);
    assert_eq!(error.stage(), Some(OpenRouterStreamStage::SseUtf8));
}
