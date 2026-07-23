use super::*;
use crate::backend::BackendRuntimeEvent;
use crate::runtime::{finish_work_and_latch_shutdown, RuntimeCommand};

#[tokio::test]
async fn ready_intents_and_events_follow_rotating_priority() {
    let (_shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
    let (intent_tx, mut intents) = tokio::sync::mpsc::channel(2);
    intent_tx
        .send(RuntimeCommand::Intent(Intent::Help))
        .await
        .unwrap();

    let event_first = next_open_work(
        &mut shutdowns,
        &mut intents,
        std::future::ready(BackendRuntimeEvent::Codex(Some(Ok(
            SessionEvent::UnknownNotification("ready-event".to_owned()),
        )))),
        true,
    )
    .await;
    assert!(matches!(
        event_first,
        RuntimeWork::Event(BackendRuntimeEvent::Codex(Some(Ok(
            SessionEvent::UnknownNotification(method)
        ))))
            if method == "ready-event"
    ));

    let intent_next = next_open_work(
        &mut shutdowns,
        &mut intents,
        std::future::ready(BackendRuntimeEvent::Codex(Some(Ok(
            SessionEvent::UnknownNotification("second-event".to_owned()),
        )))),
        false,
    )
    .await;
    assert!(matches!(
        intent_next,
        RuntimeWork::Command(Some(RuntimeCommand::Intent(Intent::Help)))
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
        intent_tx
            .send(RuntimeCommand::Intent(Intent::Help))
            .await
            .unwrap();
        let _ = release_tx.send(());
    };

    let (completion, ()) = tokio::join!(
        finish_event_or_shutdown(&mut shutdowns, process),
        queue_intent
    );
    assert!(matches!(completion, EventCompletion::Finished(Ok(true))));

    let next = next_open_work(&mut shutdowns, &mut intents, std::future::pending(), false).await;
    assert!(matches!(
        next,
        RuntimeWork::Command(Some(RuntimeCommand::Intent(Intent::Help)))
    ));
}

#[tokio::test]
async fn shutdown_latches_without_dropping_already_accepted_work() {
    let (shutdown_tx, mut shutdowns) = tokio::sync::mpsc::channel(1);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed_by_work = completed.clone();
    let work = async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        completed_by_work.store(true, std::sync::atomic::Ordering::SeqCst);
        42
    };
    let request_shutdown = async move {
        let _ = started_rx.await;
        shutdown_tx.send(()).await.unwrap();
        let _ = release_tx.send(());
    };

    let ((result, latched), ()) = tokio::join!(
        finish_work_and_latch_shutdown(&mut shutdowns, work),
        request_shutdown
    );
    assert_eq!(result, 42);
    assert!(latched);
    assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
}
