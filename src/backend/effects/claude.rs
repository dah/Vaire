use super::*;
use crate::backend::claude_runtime::{
    auth_operation_error, claude_error, claude_store_error, inspect_runtime_auth, now_ms,
    selected_claude_alias,
};

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn refresh_claude_effect(&mut self) -> Vec<Effect> {
        if !matches!(
            self.state.claude.auth_operation,
            crate::app::ClaudeAuthOperation::Idle
        ) {
            self.state.notice =
                Some("a Claude authentication operation is already active".to_owned());
            return Vec::new();
        }
        let Some(runtime) = &mut self.claude else {
            self.record_claude_unavailable("Claude Code runtime is unavailable");
            return Vec::new();
        };
        let operation_id = runtime.operation_id();
        self.state.claude.auth = ClaudeAuthStatus::Unverified;
        self.state.claude.auth_operation =
            crate::app::ClaudeAuthOperation::Checking { operation_id };
        match inspect_runtime_auth(runtime).await {
            Ok(auth) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(auth)));
                if auth == ClaudeAuthStatus::Subscription {
                    effects.extend(self.block_unscoped_saved_claude_after_auth());
                }
                effects
            }
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
        }
    }

    pub(super) fn login_claude_effect(&mut self) -> Vec<Effect> {
        if !matches!(
            self.state.claude.auth_operation,
            crate::app::ClaudeAuthOperation::Idle
        ) {
            self.state.notice =
                Some("a Claude authentication operation is already active".to_owned());
            return Vec::new();
        }
        if self.state.claude.auth == ClaudeAuthStatus::Subscription {
            self.state.notice = Some("Claude is already signed in with a subscription".to_owned());
            return Vec::new();
        }
        if self.state.turn.is_active() {
            self.state.notice = Some(
                "wait for or interrupt the active turn before signing in to Claude".to_owned(),
            );
            return Vec::new();
        }
        let Some(runtime) = &mut self.claude else {
            self.record_claude_unavailable("Claude Code runtime is unavailable");
            return Vec::new();
        };
        let request = crate::app::ClaudeAuthRequest {
            operation_id: runtime.operation_id(),
            action: crate::claude::ClaudeAuthAction::Login,
        };
        self.state
            .reduce(Action::Event(DomainEvent::ClaudeAuthRequested(request)))
    }

    pub async fn complete_claude_auth(
        &mut self,
        request: crate::app::ClaudeAuthRequest,
        result: Result<(), crate::claude::ClaudeRuntimeError>,
    ) -> Vec<Effect> {
        if self.state.pending_claude_auth_request() != Some(&request) {
            return Vec::new();
        }
        self.state.claude.auth_operation = crate::app::ClaudeAuthOperation::Checking {
            operation_id: request.operation_id,
        };
        if let Err(error) = result {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    auth_operation_error(error),
                )));
        }
        let Some(runtime) = &self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(ClaudeFailureStage::Auth, ClaudeFailureCategory::Unavailable),
                )));
        };
        match inspect_runtime_auth(runtime).await {
            Ok(auth) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(auth)));
                match request.action {
                    crate::claude::ClaudeAuthAction::Login
                        if auth == ClaudeAuthStatus::Subscription =>
                    {
                        effects.extend(self.block_unscoped_saved_claude_after_auth());
                    }
                    crate::claude::ClaudeAuthAction::Login => {
                        self.state.notice = Some(
                            "Claude Code did not finish a Claude subscription sign-in; retry /login"
                                .to_owned(),
                        );
                    }
                    crate::claude::ClaudeAuthAction::Logout
                        if auth != ClaudeAuthStatus::SignedOut =>
                    {
                        self.state.notice = Some(
                            "Claude Code still reports an authenticated account after logout"
                                .to_owned(),
                        );
                    }
                    crate::claude::ClaudeAuthAction::Logout => {}
                }
                effects
            }
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
        }
    }

    pub(super) async fn logout_claude_effect(&mut self) -> Vec<Effect> {
        if !matches!(
            self.state.claude.auth_operation,
            crate::app::ClaudeAuthOperation::Idle
        ) {
            self.state.notice =
                Some("a Claude authentication operation is already active".to_owned());
            return Vec::new();
        }
        let Some(runtime) = &mut self.claude else {
            self.record_claude_unavailable("Claude Code runtime is unavailable");
            return Vec::new();
        };
        let drained = runtime.service.interrupt_and_drain().await;
        let mut produced = Vec::new();
        for event in drained {
            produced.extend(self.reduce_claude_service_event(event));
        }
        let Some(runtime) = &mut self.claude else {
            return produced;
        };
        let request = crate::app::ClaudeAuthRequest {
            operation_id: runtime.operation_id(),
            action: crate::claude::ClaudeAuthAction::Logout,
        };
        produced.extend(
            self.state
                .reduce(Action::Event(DomainEvent::ClaudeAuthRequested(request))),
        );
        produced
    }

    pub(super) async fn start_new_claude_session_effect(&mut self) -> Vec<Effect> {
        let alias = selected_claude_alias(&self.state);
        let Some(runtime) = &self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Store,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )));
        };
        match runtime.service.create_session(alias, now_ms()).await {
            Ok((session_id, commit))
                if commit.source == crate::storage::CommitStatus::Verified =>
            {
                match self.persist_claude_pointer(&session_id) {
                    Ok(Some(crate::storage::CommitStatus::Verified)) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
                            session_id,
                        }))
                        .into_iter()
                        .filter(|effect| !matches!(effect, Effect::Persist(_)))
                        .collect(),
                    Ok(Some(crate::storage::CommitStatus::CommittedUnverified)) => self
                        .state
                        .reduce(Action::Event(
                            DomainEvent::ClaudeSessionCreationUncertain {
                                session_id,
                                message: "Vairë committed the new session pointer, but could not verify its directory durability".to_owned(),
                            },
                        ))
                        .into_iter()
                        .filter(|effect| !matches!(effect, Effect::Persist(_)))
                        .collect(),
                    Ok(None) | Err(_) => self.state.reduce(Action::Event(
                        DomainEvent::ClaudeNewSessionFailed(
                            "the new session pointer could not be durably committed".to_owned(),
                        ),
                    )),
                }
            }
            Ok((_session_id, _commit)) => self.state.reduce(Action::Event(
                DomainEvent::ClaudeNewSessionFailed(
                    "the new Vairë session registration was committed, but its directory durability could not be verified; use /resume to inspect registered sessions"
                        .to_owned(),
                ),
            )),
            Err(_) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_store_error(),
                ))),
        }
    }

    pub(super) async fn switch_claude_session_effect(
        &mut self,
        session_id: crate::provider::ClaudeSessionId,
    ) -> Vec<Effect> {
        let Some(runtime) = &self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeSessionSwitchFailed {
                    session_id,
                    message: "Claude Code runtime is unavailable".to_owned(),
                }));
        };
        match runtime.service.load_session(session_id.clone()).await {
            Ok(session) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeSessionRestored {
                    session,
                    automatic: false,
                })),
            Err(error) => {
                self.state
                    .reduce(Action::Event(DomainEvent::ClaudeSessionSwitchFailed {
                        session_id,
                        message: error.to_string(),
                    }))
            }
        }
    }

    pub(super) async fn send_claude_message_effect(
        &mut self,
        text: String,
        effort: Option<ClaudeEffort>,
    ) -> Vec<Effect> {
        let auth = match &self.claude {
            Some(runtime) => inspect_runtime_auth(runtime).await,
            None => Err(claude_error(
                ClaudeFailureStage::Auth,
                ClaudeFailureCategory::Unavailable,
            )),
        };
        match auth {
            Ok(ClaudeAuthStatus::Subscription) => {}
            Ok(auth) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(auth)));
                effects.extend(self.state.reduce(Action::Event(
                    DomainEvent::ClaudeOperationFailed(claude_error(
                        ClaudeFailureStage::Auth,
                        ClaudeFailureCategory::Unavailable,
                    )),
                )));
                return effects;
            }
            Err(error) => {
                let mut effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeAuthChanged(
                        ClaudeAuthStatus::Unverified,
                    )));
                effects.extend(
                    self.state
                        .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
                );
                return effects;
            }
        }

        let session_id = match &self.state.claude.conversation {
            crate::app::ClaudeConversationState::Ready { id } => id.clone(),
            crate::app::ClaudeConversationState::None => {
                let alias = selected_claude_alias(&self.state);
                let Some(runtime) = &self.claude else {
                    return self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                            claude_error(
                                ClaudeFailureStage::Store,
                                ClaudeFailureCategory::Unavailable,
                            ),
                        )));
                };
                return match runtime.service.create_session(alias, now_ms()).await {
                    Ok((session_id, commit)) => {
                        if commit.source != crate::storage::CommitStatus::Verified {
                            let mut effects = self.state.reduce(Action::Event(
                                DomainEvent::ClaudeOperationFailed(claude_store_error()),
                            ));
                            effects.extend(self.state.reduce(Action::Event(
                                DomainEvent::ClaudeSessionCreationUncertain {
                                    session_id,
                                    message: "Vairë committed the new session registration but could not verify its directory durability".to_owned(),
                                },
                            )));
                            return effects;
                        }
                        match self.persist_claude_pointer(&session_id) {
                            Ok(Some(crate::storage::CommitStatus::Verified)) => {}
                            Ok(Some(crate::storage::CommitStatus::CommittedUnverified)) => {
                                let mut effects = self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeOperationFailed(claude_store_error()),
                                ));
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeSessionCreationUncertain {
                                        session_id,
                                        message: "Vairë committed the new session pointer, but could not verify its directory durability".to_owned(),
                                    },
                                )));
                                return effects
                                    .into_iter()
                                    .filter(|effect| !matches!(effect, Effect::Persist(_)))
                                    .collect();
                            }
                            Ok(None) | Err(_) => {
                                let mut effects = self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeOperationFailed(claude_store_error()),
                                ));
                                effects.extend(self.state.reduce(Action::Event(
                                    DomainEvent::ClaudeSessionCreationUncertain {
                                        session_id,
                                        message: "Vairë could not durably commit the new session pointer".to_owned(),
                                    },
                                )));
                                return effects;
                            }
                        }
                        let mut effects = self
                            .state
                            .reduce(Action::Event(DomainEvent::ClaudeSessionStarted {
                                session_id,
                            }))
                            .into_iter()
                            .filter(|effect| !matches!(effect, Effect::Persist(_)))
                            .collect::<Vec<_>>();
                        // The UUID registration and pointer are verified before the child can
                        // be prepared or launched.
                        effects.push(Effect::SendClaudeMessage { text, effort });
                        effects
                    }
                    Err(_) => self
                        .state
                        .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                            claude_store_error(),
                        ))),
                };
            }
            _ => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                        claude_error(
                            ClaudeFailureStage::Store,
                            ClaudeFailureCategory::Unavailable,
                        ),
                    )))
            }
        };
        let alias = selected_claude_alias(&self.state);
        let Some(runtime) = &mut self.claude else {
            return self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Spawn,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )));
        };
        let prepared = match runtime
            .service
            .prepare_turn(session_id, alias, effort, text, now_ms())
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return self
                    .state
                    .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error)))
            }
        };
        match runtime
            .service
            .launch_prepared_turn(prepared, now_ms())
            .await
        {
            Ok(()) => Vec::new(),
            Err(error) => self
                .state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(error))),
        }
    }

    pub(super) fn interrupt_claude_turn_effect(&mut self) -> Vec<Effect> {
        if self
            .claude
            .as_ref()
            .is_some_and(|runtime| runtime.service.interrupt_turn())
        {
            Vec::new()
        } else {
            self.state
                .reduce(Action::Event(DomainEvent::ClaudeOperationFailed(
                    claude_error(
                        ClaudeFailureStage::Shutdown,
                        ClaudeFailureCategory::Unavailable,
                    ),
                )))
        }
    }

    fn persist_claude_pointer(
        &mut self,
        session_id: &crate::provider::ClaudeSessionId,
    ) -> Result<Option<crate::storage::CommitStatus>, PersistenceError> {
        let mut preferences = self.state.preferences.clone();
        preferences.set_auto_resume_claude_session(Some(session_id.clone()));
        self.persist_preferences(&preferences)
    }

    fn block_unscoped_saved_claude_after_auth(&mut self) -> Vec<Effect> {
        if self.state.active_provider != ProviderId::Claude {
            return Vec::new();
        }
        let Some(session_id) = self.state.preferences.claude.auto_resume_session_id.clone() else {
            return Vec::new();
        };
        self.state
            .reduce(Action::Event(DomainEvent::ClaudeResumeFailed {
                session_id,
                message: "Claude Code does not expose a stable account identity through its supported auth status; use /resume to deliberately restore this local session or /new to start blank"
                    .to_owned(),
            }))
    }
}
