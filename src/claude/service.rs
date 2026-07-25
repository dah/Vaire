use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::provider::{ClaudeSessionId, ClaudeTurnId};
use crate::storage::CommitStatus;

use super::protocol::EVENT_QUEUE_CAPACITY;
use super::{
    ClaudeChild, ClaudeCliPolicy, ClaudeEffort, ClaudeError, ClaudeFailureCategory,
    ClaudeFailureStage, ClaudeInvocation, ClaudeModelAlias, ClaudeModelMetadata,
    ClaudeProcessError, ClaudeServiceEvent, ClaudeSessionLifecycle, ClaudeSessionStore,
    ClaudeSessionSummary, ClaudeSessionV1, ClaudeStoreError, ClaudeStreamEvent, ClaudeTurnOutcome,
    ClaudeTurnRecord,
};

const DEFAULT_TITLE: &str = "New Claude conversation";

pub struct PreparedClaudeTurn {
    session_id: ClaudeSessionId,
    turn_id: ClaudeTurnId,
    invocation: ClaudeInvocation,
    prompt: String,
    recovering_creation_uncertainty: bool,
    preparation_verified: bool,
}

#[cfg(test)]
impl PreparedClaudeTurn {
    pub(crate) fn invocation(&self) -> &ClaudeInvocation {
        &self.invocation
    }
}

struct ActiveTurn {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

pub struct ClaudeService {
    policy: ClaudeCliPolicy,
    store: Arc<dyn ClaudeSessionStore>,
    events_tx: mpsc::Sender<ClaudeServiceEvent>,
    events_rx: mpsc::Receiver<ClaudeServiceEvent>,
    active: Option<ActiveTurn>,
    shutting_down: bool,
}

impl ClaudeService {
    pub fn new(policy: ClaudeCliPolicy, store: Arc<dyn ClaudeSessionStore>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        Self {
            policy,
            store,
            events_tx,
            events_rx,
            active: None,
            shutting_down: false,
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<ClaudeSessionSummary>, ClaudeStoreError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.list_sessions())
            .await
            .map_err(|_| ClaudeStoreError::Read)?
    }

    pub async fn load_session(
        &self,
        id: ClaudeSessionId,
    ) -> Result<ClaudeSessionV1, ClaudeStoreError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.load_session(&id))
            .await
            .map_err(|_| ClaudeStoreError::Read)?
    }

    pub async fn create_session(
        &self,
        alias: ClaudeModelAlias,
        now_ms: u64,
    ) -> Result<(ClaudeSessionId, super::ClaudeSessionCommit), ClaudeStoreError> {
        let id = ClaudeSessionId::new();
        let session = ClaudeSessionV1::new(id.clone(), alias, now_ms, DEFAULT_TITLE);
        let store = Arc::clone(&self.store);
        let commit = tokio::task::spawn_blocking(move || store.save_session_with_commit(&session))
            .await
            .map_err(|_| ClaudeStoreError::Write)??;
        Ok((id, commit))
    }

    pub async fn delete_session(&self, id: ClaudeSessionId) -> Result<(), ClaudeStoreError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.delete_session(&id))
            .await
            .map_err(|_| ClaudeStoreError::Delete)?
    }

    pub async fn prepare_turn(
        &mut self,
        session_id: ClaudeSessionId,
        alias: ClaudeModelAlias,
        effort: Option<ClaudeEffort>,
        text: String,
        now_ms: u64,
    ) -> Result<PreparedClaudeTurn, ClaudeError> {
        self.reap_finished().await;
        if self.shutting_down || self.active.is_some() || text.is_empty() {
            return Err(ClaudeError::new(
                ClaudeFailureStage::Store,
                ClaudeFailureCategory::Unavailable,
            ));
        }
        let turn_id = ClaudeTurnId::new();
        let store = Arc::clone(&self.store);
        let lookup = session_id.clone();
        let record_id = turn_id.clone();
        let prompt = text.clone();
        let save_result = tokio::task::spawn_blocking(move || {
            let mut session = store.load_session_for_update(&lookup)?;
            let (invocation, recovering_creation_uncertainty) = match session.lifecycle {
                ClaudeSessionLifecycle::Fresh => {
                    session.lifecycle = ClaudeSessionLifecycle::CreationPending;
                    (
                        ClaudeInvocation::NewSession {
                            session_id: lookup.clone(),
                            model: alias,
                            effort,
                        },
                        false,
                    )
                }
                ClaudeSessionLifecycle::CreationPending => return Err(ClaudeStoreError::Corrupt),
                ClaudeSessionLifecycle::Established => (
                    ClaudeInvocation::ResumeSession {
                        session_id: lookup.clone(),
                        model: alias,
                        effort,
                    },
                    false,
                ),
                ClaudeSessionLifecycle::CreationUncertain => (
                    ClaudeInvocation::ResumeSession {
                        session_id: lookup.clone(),
                        model: alias,
                        effort,
                    },
                    true,
                ),
            };
            session.selected_model = alias;
            session.updated_at_ms = now_ms;
            session.turns.push(ClaudeTurnRecord {
                id: record_id,
                requested_model: alias,
                user_text: text,
                assistant_text: None,
                incomplete_assistant_text: None,
                outcome: ClaudeTurnOutcome::InProgress,
            });
            let commit_error = store
                .save_session_with_commit(&session)
                .and_then(require_verified_source)
                .err();
            if let Some(error) = commit_error {
                if let Some(turn) = session.turns.last_mut() {
                    turn.outcome = ClaudeTurnOutcome::Interrupted;
                }
                if session.lifecycle == ClaudeSessionLifecycle::CreationPending {
                    session.lifecycle = ClaudeSessionLifecycle::Fresh;
                }
                let _ = store.save_session_with_commit(&session);
                if recovering_creation_uncertainty {
                    return Ok((invocation, true, false));
                }
                return Err(error);
            }
            Ok((invocation, recovering_creation_uncertainty, true))
        })
        .await
        .map_err(|_| store_failure())?;
        let (invocation, recovering_creation_uncertainty, preparation_verified) =
            save_result.map_err(|_| store_failure())?;
        Ok(PreparedClaudeTurn {
            session_id,
            turn_id,
            invocation,
            prompt,
            recovering_creation_uncertainty,
            preparation_verified,
        })
    }

    pub async fn abandon_prepared_turn(
        &self,
        prepared: PreparedClaudeTurn,
        now_ms: u64,
    ) -> Result<bool, ClaudeStoreError> {
        settle_record(
            Arc::clone(&self.store),
            prepared.session_id,
            prepared.turn_id,
            TurnSettlement {
                outcome: ClaudeTurnOutcome::Interrupted,
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_was_attempted: false,
                now_ms,
            },
        )
        .await
    }

    pub async fn launch_prepared_turn(
        &mut self,
        prepared: PreparedClaudeTurn,
        now_ms: u64,
    ) -> Result<(), ClaudeError> {
        self.reap_finished().await;
        if self.shutting_down || self.active.is_some() {
            let _ = self.abandon_prepared_turn(prepared, now_ms).await;
            return Err(ClaudeError::new(
                ClaudeFailureStage::Spawn,
                ClaudeFailureCategory::Unavailable,
            ));
        }
        if !prepared.preparation_verified {
            self.finish_prepared_before_launch(prepared, now_ms, store_failure())
                .await;
            return Ok(());
        }
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let policy = self.policy.clone();
        let store = Arc::clone(&self.store);
        let events = self.events_tx.clone();
        let task = tokio::spawn(async move {
            run_turn(policy, store, events, prepared, task_cancellation, now_ms).await;
        });
        self.active = Some(ActiveTurn { cancellation, task });
        Ok(())
    }

    async fn finish_prepared_before_launch(
        &self,
        prepared: PreparedClaudeTurn,
        now_ms: u64,
        failure: ClaudeError,
    ) {
        let session_id = prepared.session_id;
        let turn_id = prepared.turn_id;
        let _ = self
            .events_tx
            .send(ClaudeServiceEvent::TurnStarted {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            })
            .await;
        let store_result = settle_record(
            Arc::clone(&self.store),
            session_id.clone(),
            turn_id.clone(),
            TurnSettlement {
                outcome: ClaudeTurnOutcome::Interrupted,
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_was_attempted: false,
                now_ms,
            },
        )
        .await;
        let creation_uncertain = prepared.recovering_creation_uncertainty
            || store_result.as_ref().ok().copied().unwrap_or(false);
        let failure = store_result
            .as_ref()
            .err()
            .map(|_| store_failure())
            .unwrap_or(failure);
        let _ = self
            .events_tx
            .send(ClaudeServiceEvent::TurnFinished {
                session_id,
                turn_id,
                outcome: ClaudeTurnOutcome::Failed,
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_uncertain,
                failure: Some(failure),
            })
            .await;
    }

    pub fn interrupt_turn(&self) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        active.cancellation.cancel();
        true
    }

    /// Cancels and reaps the active child without permanently shutting down the service.
    pub async fn interrupt_and_drain(&mut self) -> Vec<ClaudeServiceEvent> {
        self.cancel_active_and_drain().await
    }

    pub async fn next_event(&mut self) -> Option<ClaudeServiceEvent> {
        self.events_rx.recv().await
    }

    pub async fn shutdown(&mut self) -> Vec<ClaudeServiceEvent> {
        self.shutting_down = true;
        self.cancel_active_and_drain().await
    }

    async fn cancel_active_and_drain(&mut self) -> Vec<ClaudeServiceEvent> {
        let mut drained = Vec::new();
        if let Some(mut active) = self.active.take() {
            active.cancellation.cancel();
            loop {
                tokio::select! {
                    _ = &mut active.task => break,
                    event = self.events_rx.recv() => {
                        let Some(event) = event else {
                            let _ = active.task.await;
                            break;
                        };
                        drained.push(event);
                    }
                }
            }
        }
        while let Ok(event) = self.events_rx.try_recv() {
            drained.push(event);
        }
        drained
    }

    async fn reap_finished(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.task.is_finished())
        {
            if let Some(active) = self.active.take() {
                let _ = active.task.await;
            }
        }
    }
}

async fn run_turn(
    policy: ClaudeCliPolicy,
    store: Arc<dyn ClaudeSessionStore>,
    events: mpsc::Sender<ClaudeServiceEvent>,
    prepared: PreparedClaudeTurn,
    cancellation: CancellationToken,
    now_ms: u64,
) {
    let session_id = prepared.session_id;
    let turn_id = prepared.turn_id;
    let is_new_session = matches!(&prepared.invocation, ClaudeInvocation::NewSession { .. });
    let recovering_creation_uncertainty = prepared.recovering_creation_uncertainty;
    let creation_requires_establishment = is_new_session || recovering_creation_uncertainty;
    if events
        .send(ClaudeServiceEvent::TurnStarted {
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
        })
        .await
        .is_err()
    {
        let _ = settle_record(
            store,
            session_id,
            turn_id,
            TurnSettlement {
                outcome: ClaudeTurnOutcome::Interrupted,
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_was_attempted: false,
                now_ms,
            },
        )
        .await;
        return;
    }
    let mut child = match ClaudeChild::spawn(
        &policy,
        &prepared.invocation,
        session_id.clone(),
        &prepared.prompt,
        &cancellation,
    )
    .await
    {
        Ok(child) => child,
        Err(error)
            if matches!(
                error,
                ClaudeProcessError::Interrupted | ClaudeProcessError::InterruptedAfterSpawn
            ) =>
        {
            let creation_was_attempted = matches!(error, ClaudeProcessError::InterruptedAfterSpawn);
            let store_result = settle_record(
                store,
                session_id.clone(),
                turn_id.clone(),
                TurnSettlement {
                    outcome: ClaudeTurnOutcome::Interrupted,
                    assistant_text: None,
                    incomplete_assistant_text: None,
                    creation_was_attempted,
                    now_ms,
                },
            )
            .await;
            let creation_uncertain = store_result.as_ref().ok().copied().unwrap_or(
                recovering_creation_uncertainty || (is_new_session && creation_was_attempted),
            );
            let store_failed = store_result.is_err();
            let _ = events
                .send(ClaudeServiceEvent::TurnFinished {
                    session_id,
                    turn_id,
                    outcome: if store_failed {
                        ClaudeTurnOutcome::Failed
                    } else {
                        ClaudeTurnOutcome::Interrupted
                    },
                    assistant_text: None,
                    incomplete_assistant_text: None,
                    creation_uncertain,
                    failure: store_failed.then(store_failure),
                })
                .await;
            return;
        }
        Err(error) => {
            let creation_was_attempted = !matches!(error, ClaudeProcessError::Spawn);
            let _ = settle_after_failure(
                store,
                &events,
                session_id,
                turn_id,
                FailureSettlement {
                    failure: process_failure(error),
                    incomplete_assistant_text: None,
                    creation_was_attempted,
                    creation_uncertain_on_store_error: recovering_creation_uncertainty
                        || (is_new_session && creation_was_attempted),
                    now_ms,
                },
            )
            .await;
            return;
        }
    };

    let mut creation_established = false;
    let mut terminal_success = None;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let process_result = child.interrupt().await;
                let process_failure = process_result.err().map(process_failure);
                let store_result = settle_record(
                    Arc::clone(&store),
                    session_id.clone(),
                    turn_id.clone(),
                    TurnSettlement {
                        outcome: ClaudeTurnOutcome::Interrupted,
                        assistant_text: None,
                        incomplete_assistant_text: None,
                        creation_was_attempted: true,
                        now_ms,
                    },
                ).await;
                let creation_uncertain = store_result
                    .as_ref()
                    .ok()
                    .copied()
                    .unwrap_or(creation_requires_establishment && !creation_established);
                let store_failure = store_result.err().map(|_| store_failure());
                let operation_failed = process_failure.is_some() || store_failure.is_some();
                let _ = events.send(ClaudeServiceEvent::TurnFinished {
                    session_id,
                    turn_id,
                    outcome: if operation_failed {
                        ClaudeTurnOutcome::Failed
                    } else {
                        ClaudeTurnOutcome::Interrupted
                    },
                    assistant_text: None,
                    incomplete_assistant_text: None,
                    creation_uncertain,
                    failure: process_failure.or(store_failure),
                }).await;
                return;
            }
            event = child.next_event() => {
                match event {
                    Ok(Some(ClaudeStreamEvent::Initialized { model, .. })) => {
                        if persist_initialized(
                            Arc::clone(&store),
                            session_id.clone(),
                            model.clone(),
                            now_ms,
                        ).await.is_err() {
                            let incomplete = nonempty_partial(child.assistant_text());
                            let _ = child.interrupt().await;
                            let _ = settle_after_failure(
                                store,
                                &events,
                                session_id,
                                turn_id,
                                FailureSettlement {
                                    failure: store_failure(),
                                    incomplete_assistant_text: incomplete,
                                    creation_was_attempted: true,
                                    creation_uncertain_on_store_error:
                                        creation_requires_establishment,
                                    now_ms,
                                },
                            ).await;
                            return;
                        }
                        creation_established = true;
                        let _ = events.send(ClaudeServiceEvent::Initialized {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            model,
                        }).await;
                    }
                    Ok(Some(ClaudeStreamEvent::TextDelta { delta })) => {
                        let _ = events.send(ClaudeServiceEvent::TextDelta {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            delta,
                        }).await;
                    }
                    Ok(Some(ClaudeStreamEvent::Terminal { success, final_text })) => {
                        terminal_success = Some((success, final_text));
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let incomplete = nonempty_partial(child.assistant_text());
                        let _ = child.interrupt().await;
                        let _ = settle_after_failure(
                            store,
                            &events,
                            session_id,
                            turn_id,
                            FailureSettlement {
                                failure: process_failure(error),
                                incomplete_assistant_text: incomplete,
                                creation_was_attempted: true,
                                creation_uncertain_on_store_error: creation_requires_establishment
                                    && !creation_established,
                                now_ms,
                            },
                        ).await;
                        return;
                    }
                }
            }
        }
    }

    let streamed_partial = nonempty_partial(child.assistant_text());
    let exit = child.finish(&cancellation).await;
    if matches!(&exit, Err(ClaudeProcessError::Interrupted)) {
        let store_result = settle_record(
            store,
            session_id.clone(),
            turn_id.clone(),
            TurnSettlement {
                outcome: ClaudeTurnOutcome::Interrupted,
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_was_attempted: true,
                now_ms,
            },
        )
        .await;
        let creation_uncertain = store_result
            .as_ref()
            .ok()
            .copied()
            .unwrap_or(creation_requires_establishment && !creation_established);
        let store_failed = store_result.is_err();
        let _ = events
            .send(ClaudeServiceEvent::TurnFinished {
                session_id,
                turn_id,
                outcome: if store_failed {
                    ClaudeTurnOutcome::Failed
                } else {
                    ClaudeTurnOutcome::Interrupted
                },
                assistant_text: None,
                incomplete_assistant_text: None,
                creation_uncertain,
                failure: store_failed.then(store_failure),
            })
            .await;
        return;
    }
    let completed = matches!(terminal_success, Some((true, _))) && exit.is_ok();
    let (outcome, assistant, incomplete, failure) = if completed {
        let assistant = terminal_success.and_then(|(_, text)| text);
        (ClaudeTurnOutcome::Completed, assistant, None, None)
    } else {
        let failure = exit.err().map(process_failure).unwrap_or_else(|| {
            ClaudeError::new(
                ClaudeFailureStage::Protocol,
                ClaudeFailureCategory::Protocol,
            )
        });
        (
            ClaudeTurnOutcome::Failed,
            None,
            streamed_partial.clone(),
            Some(failure),
        )
    };
    let store_result = settle_record(
        store,
        session_id.clone(),
        turn_id.clone(),
        TurnSettlement {
            outcome,
            assistant_text: assistant.clone(),
            incomplete_assistant_text: incomplete.clone(),
            creation_was_attempted: true,
            now_ms,
        },
    )
    .await;
    let creation_uncertain = store_result
        .as_ref()
        .ok()
        .copied()
        .unwrap_or(creation_requires_establishment && !creation_established);
    let store_failed = store_result.is_err();
    let event_outcome = if store_failed {
        ClaudeTurnOutcome::Failed
    } else {
        outcome
    };
    let event_incomplete = if store_failed {
        assistant
            .clone()
            .or(incomplete.clone())
            .or(streamed_partial)
    } else {
        incomplete
    };
    let event_assistant = if store_failed { None } else { assistant };
    let event_failure = if store_failed {
        Some(store_failure())
    } else {
        failure
    };
    let _ = events
        .send(ClaudeServiceEvent::TurnFinished {
            session_id,
            turn_id,
            outcome: event_outcome,
            assistant_text: event_assistant,
            incomplete_assistant_text: event_incomplete,
            creation_uncertain,
            failure: event_failure,
        })
        .await;
}

async fn persist_initialized(
    store: Arc<dyn ClaudeSessionStore>,
    session_id: ClaudeSessionId,
    model: ClaudeModelMetadata,
    now_ms: u64,
) -> Result<(), ClaudeStoreError> {
    tokio::task::spawn_blocking(move || {
        let mut session = store.load_session_for_update(&session_id)?;
        session.lifecycle = ClaudeSessionLifecycle::Established;
        session.resolved_model = Some(model);
        session.updated_at_ms = now_ms;
        let commit = store.save_session_with_commit(&session)?;
        require_verified_source(commit)
    })
    .await
    .map_err(|_| ClaudeStoreError::Write)?
}

struct TurnSettlement {
    outcome: ClaudeTurnOutcome,
    assistant_text: Option<String>,
    incomplete_assistant_text: Option<String>,
    creation_was_attempted: bool,
    now_ms: u64,
}

async fn settle_record(
    store: Arc<dyn ClaudeSessionStore>,
    session_id: ClaudeSessionId,
    turn_id: ClaudeTurnId,
    settlement: TurnSettlement,
) -> Result<bool, ClaudeStoreError> {
    tokio::task::spawn_blocking(move || {
        let TurnSettlement {
            outcome,
            assistant_text,
            incomplete_assistant_text,
            creation_was_attempted,
            now_ms,
        } = settlement;
        let mut session = store.load_session_for_update(&session_id)?;
        let turn = session
            .turns
            .iter_mut()
            .find(|turn| turn.id == turn_id)
            .ok_or(ClaudeStoreError::Corrupt)?;
        turn.outcome = outcome;
        turn.assistant_text = if outcome == ClaudeTurnOutcome::Completed {
            assistant_text
        } else {
            None
        };
        turn.incomplete_assistant_text = if outcome == ClaudeTurnOutcome::Failed {
            incomplete_assistant_text
        } else {
            None
        };
        let creation_pending = session.lifecycle == ClaudeSessionLifecycle::CreationPending;
        if creation_pending {
            session.lifecycle = if creation_was_attempted {
                ClaudeSessionLifecycle::CreationUncertain
            } else {
                ClaudeSessionLifecycle::Fresh
            };
        }
        let creation_uncertain = session.lifecycle == ClaudeSessionLifecycle::CreationUncertain;
        session.updated_at_ms = now_ms;
        let commit = store.save_session_with_commit(&session)?;
        require_verified_source(commit)?;
        Ok(creation_uncertain)
    })
    .await
    .map_err(|_| ClaudeStoreError::Write)?
}

struct FailureSettlement {
    failure: ClaudeError,
    incomplete_assistant_text: Option<String>,
    creation_was_attempted: bool,
    creation_uncertain_on_store_error: bool,
    now_ms: u64,
}

async fn settle_after_failure(
    store: Arc<dyn ClaudeSessionStore>,
    events: &mpsc::Sender<ClaudeServiceEvent>,
    session_id: ClaudeSessionId,
    turn_id: ClaudeTurnId,
    settlement: FailureSettlement,
) -> Result<(), ClaudeStoreError> {
    let FailureSettlement {
        failure,
        incomplete_assistant_text,
        creation_was_attempted,
        creation_uncertain_on_store_error,
        now_ms,
    } = settlement;
    let store_result = settle_record(
        store,
        session_id.clone(),
        turn_id.clone(),
        TurnSettlement {
            outcome: ClaudeTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: incomplete_assistant_text.clone(),
            creation_was_attempted,
            now_ms,
        },
    )
    .await;
    let creation_uncertain =
        creation_uncertain_on_store_error || store_result.as_ref().ok().copied().unwrap_or(false);
    let failure = store_result
        .as_ref()
        .err()
        .map(|_| store_failure())
        .unwrap_or(failure);
    let _ = events
        .send(ClaudeServiceEvent::TurnFinished {
            session_id,
            turn_id,
            outcome: ClaudeTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text,
            creation_uncertain,
            failure: Some(failure),
        })
        .await;
    store_result.map(|_| ())
}

fn nonempty_partial(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn process_failure(error: ClaudeProcessError) -> ClaudeError {
    use super::ClaudeProtocolError;
    match error {
        ClaudeProcessError::Spawn => ClaudeError::new(
            ClaudeFailureStage::Spawn,
            ClaudeFailureCategory::Unavailable,
        ),
        ClaudeProcessError::Stdin => {
            ClaudeError::new(ClaudeFailureStage::Stdin, ClaudeFailureCategory::Io)
        }
        ClaudeProcessError::Stdout => {
            ClaudeError::new(ClaudeFailureStage::Stdout, ClaudeFailureCategory::Io)
        }
        ClaudeProcessError::StderrLimit
        | ClaudeProcessError::Protocol(ClaudeProtocolError::ResourceLimit) => ClaudeError::new(
            ClaudeFailureStage::Protocol,
            ClaudeFailureCategory::ResourceLimit,
        ),
        ClaudeProcessError::Protocol(_) => ClaudeError::new(
            ClaudeFailureStage::Protocol,
            ClaudeFailureCategory::Protocol,
        ),
        ClaudeProcessError::NonZeroExit => {
            ClaudeError::new(ClaudeFailureStage::Exit, ClaudeFailureCategory::NonZeroExit)
        }
        ClaudeProcessError::Reap => {
            ClaudeError::new(ClaudeFailureStage::Reap, ClaudeFailureCategory::Io)
        }
        ClaudeProcessError::Interrupted | ClaudeProcessError::InterruptedAfterSpawn => {
            ClaudeError::new(
                ClaudeFailureStage::Shutdown,
                ClaudeFailureCategory::Interrupted,
            )
        }
    }
}

fn require_verified_source(commit: super::ClaudeSessionCommit) -> Result<(), ClaudeStoreError> {
    if commit.source == CommitStatus::Verified {
        Ok(())
    } else {
        Err(ClaudeStoreError::Write)
    }
}

fn store_failure() -> ClaudeError {
    ClaudeError::new(
        ClaudeFailureStage::Store,
        ClaudeFailureCategory::CorruptStore,
    )
}
