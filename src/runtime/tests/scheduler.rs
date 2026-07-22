use super::*;

#[tokio::test]
async fn ready_intents_and_events_follow_rotating_priority() {
    let (_shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
    let (intent_tx, mut intents) = tokio::sync::mpsc::channel(2);
    intent_tx.send(Intent::Help).await.unwrap();

    let event_first = next_open_work(
        &mut shutdowns,
        &mut intents,
        std::future::ready(Some(Ok(SessionEvent::UnknownNotification(
            "ready-event".to_owned(),
        )))),
        true,
    )
    .await;
    assert!(matches!(
        event_first,
        RuntimeWork::Event(Some(Ok(SessionEvent::UnknownNotification(method))))
            if method == "ready-event"
    ));

    let intent_next = next_open_work(
        &mut shutdowns,
        &mut intents,
        std::future::ready(Some(Ok(SessionEvent::UnknownNotification(
            "second-event".to_owned(),
        )))),
        false,
    )
    .await;
    assert!(matches!(
        intent_next,
        RuntimeWork::Intent(Some(Intent::Help))
    ));
}

#[tokio::test]
async fn queued_intent_cannot_cancel_processing_of_an_already_received_event() {
    let (_shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
    let (intent_tx, mut intents) = tokio::sync::mpsc::channel(1);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();

    // This delayed processor models account/updated waiting for its follow-up account/read.
    // User input becomes ready only after the event has already been consumed.
    let process = async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        Ok(true)
    };
    let queue_intent = async move {
        let _ = started_rx.await;
        intent_tx.send(Intent::Help).await.unwrap();
        let _ = release_tx.send(());
    };

    let (completion, ()) = tokio::join!(
        finish_event_or_shutdown(&mut shutdowns, process),
        queue_intent
    );
    assert!(matches!(completion, EventCompletion::Finished(Ok(true))));

    let next = next_open_work(&mut shutdowns, &mut intents, std::future::pending(), false).await;
    assert!(matches!(next, RuntimeWork::Intent(Some(Intent::Help))));
}
