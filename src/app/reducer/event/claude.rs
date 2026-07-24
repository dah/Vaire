use super::*;

const CLAUDE_ASSISTANT_ITEM_ID: &str = "claude-assistant";

impl AppState {
    pub(super) fn reduce_claude_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::ClaudeStartup { availability, auth } => {
                self.claude.availability = availability;
                self.claude.auth = auth;
                self.claude.auth_operation = ClaudeAuthOperation::Idle;
                if self.active_provider == ProviderId::Claude {
                    self.selected_reasoning = None;
                    if self.selected_model.is_none() {
                        self.selected_model = self
                            .preferences
                            .claude
                            .selected_model_alias
                            .and_then(|alias| ModelKey::claude(alias.as_str()).ok())
                            .or_else(|| ModelKey::claude(ClaudeModelAlias::Default.as_str()).ok());
                    }
                    self.sync_active_selection_preferences();
                }
            }
            DomainEvent::ClaudeAuthRequested(request) => {
                // Native login/logout may have changed Keychain state before the child settles.
                // Drop stale authority immediately and restore it only after a correlated probe.
                self.claude.auth = ClaudeAuthStatus::Unverified;
                self.claude.auth_operation = ClaudeAuthOperation::AwaitingTerminal { request };
                self.popup = None;
                self.notice = Some(match request.action {
                    crate::claude::ClaudeAuthAction::Login => {
                        "Complete Claude subscription sign-in in your browser…".to_owned()
                    }
                    crate::claude::ClaudeAuthAction::Logout => {
                        "Signing out of the system Claude Code login…".to_owned()
                    }
                });
            }
            DomainEvent::ClaudeAuthChanged(auth) => {
                self.claude.auth = auth;
                self.claude.auth_operation = ClaudeAuthOperation::Idle;
                if auth == ClaudeAuthStatus::SignedOut {
                    self.pending_new_claude_session = false;
                }
                self.notice = Some(match auth {
                    ClaudeAuthStatus::Subscription => {
                        "Claude subscription is connected".to_owned()
                    }
                    ClaudeAuthStatus::SignedOut => "Claude is signed out".to_owned(),
                    ClaudeAuthStatus::Unsupported => {
                        "Claude is using an unsupported authentication source; sign in with a Claude subscription using /login"
                            .to_owned()
                    }
                    ClaudeAuthStatus::Unverified => {
                        "Claude subscription authentication could not be verified".to_owned()
                    }
                    ClaudeAuthStatus::CliUnavailable => {
                        "Claude Code CLI is unavailable".to_owned()
                    }
                });
            }
            DomainEvent::ClaudeOperationFailed(error) => {
                let control_operation = self.claude.auth_operation;
                let control_operation_active =
                    !matches!(control_operation, ClaudeAuthOperation::Idle);
                self.claude.auth_operation = ClaudeAuthOperation::Idle;
                self.pending_new_claude_session = false;
                let message = match control_operation {
                    ClaudeAuthOperation::Checking { .. } => format!(
                        "Claude subscription status check failed ({:?}/{:?})",
                        error.stage, error.category
                    ),
                    ClaudeAuthOperation::AwaitingTerminal { request } => match request.action {
                        crate::claude::ClaudeAuthAction::Login => format!(
                            "Claude subscription sign-in failed ({:?}/{:?})",
                            error.stage, error.category
                        ),
                        crate::claude::ClaudeAuthAction::Logout => format!(
                            "System Claude Code sign-out failed ({:?}/{:?})",
                            error.stage, error.category
                        ),
                    },
                    ClaudeAuthOperation::Idle => format!(
                        "Claude operation failed ({:?}/{:?})",
                        error.stage, error.category
                    ),
                };
                if control_operation_active {
                    self.notice = Some(message);
                    return Vec::new();
                }
                let turn_id = match &self.turn {
                    TurnState::ClaudeStreaming { turn_id, .. } => Some(turn_id.as_str().to_owned()),
                    TurnState::Starting if self.active_provider == ProviderId::Claude => None,
                    _ => {
                        self.notice = Some(message);
                        return Vec::new();
                    }
                };
                self.turn = TurnState::Failed {
                    turn_id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::ClaudeSessionStarted { session_id } => {
                if self.active_provider != ProviderId::Claude
                    || (!self.pending_new_claude_session
                        && !matches!(self.turn, TurnState::Starting))
                {
                    return Vec::new();
                }
                let explicit_new = self.pending_new_claude_session;
                self.pending_new_claude_session = false;
                self.claude.conversation = ClaudeConversationState::Ready {
                    id: session_id.clone(),
                };
                self.claude.resolved_model = None;
                if explicit_new {
                    self.turn = TurnState::Idle;
                    self.clear_transcript();
                    self.thinking.clear_content();
                    self.reset_context_window();
                    self.notice = Some("Started a new Claude conversation".to_owned());
                }
                self.preferences
                    .set_auto_resume_claude_session(Some(session_id));
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ClaudeNewSessionFailed(message) => {
                if !self.pending_new_claude_session {
                    return Vec::new();
                }
                self.pending_new_claude_session = false;
                self.notice = Some(format!(
                    "Could not start a new Claude conversation; the current conversation was preserved: {message}"
                ));
            }
            DomainEvent::ClaudeSessionCreationUncertain {
                session_id,
                message,
            } => {
                if self.active_provider != ProviderId::Claude {
                    return Vec::new();
                }
                let explicit_new = self.pending_new_claude_session;
                self.pending_new_claude_session = false;
                if explicit_new {
                    self.turn = TurnState::Idle;
                    self.clear_transcript();
                    self.thinking.clear_content();
                    self.reset_context_window();
                    self.claude.resolved_model = None;
                }
                self.claude.conversation = ClaudeConversationState::CreationUncertain {
                    id: session_id.clone(),
                    message: message.clone(),
                };
                self.preferences
                    .set_auto_resume_claude_session(Some(session_id));
                self.notice = Some(format!(
                    "Claude session creation is uncertain; use /resume or /new: {message}"
                ));
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ClaudeSessionRestored { session, automatic } => {
                let session_id = session.session_id.clone();
                let picker_requested = !automatic
                    && self.conversation_popup().is_some_and(|picker| {
                        matches!(
                            &picker.phase,
                            ThreadPickerPhase::Resuming {
                                provider: ProviderId::Claude,
                                id,
                            } if id == session_id.as_str()
                        )
                    });
                let automatic_requested = automatic
                    && self.active_provider == ProviderId::Claude
                    && self.preferences.claude.auto_resume_session_id.as_ref() == Some(&session_id);
                if !picker_requested && !automatic_requested {
                    return Vec::new();
                }

                let model = ModelKey::claude(session.selected_model.as_str())
                    .expect("stored Claude aliases are validated");
                if !self.commit_provider_selection(ProviderId::Claude, model, None) {
                    if picker_requested {
                        if let Some(picker) = self.conversation_popup_mut() {
                            picker.phase = ThreadPickerPhase::Ready;
                            picker.message = Some(
                                "The saved Claude alias is unavailable; the active conversation was preserved"
                                    .to_owned(),
                            );
                        }
                    } else {
                        self.claude.conversation = ClaudeConversationState::ResumeFailed {
                            id: session_id,
                            message: "saved Claude alias is unavailable".to_owned(),
                        };
                    }
                    return Vec::new();
                }

                let automatic_creation_uncertain = automatic
                    && matches!(
                        session.lifecycle,
                        crate::claude::ClaudeSessionLifecycle::CreationPending
                            | crate::claude::ClaudeSessionLifecycle::CreationUncertain
                    );
                self.thread = ThreadState::None;
                self.openrouter.conversation = OpenRouterConversationState::None;
                self.claude.conversation = if automatic_creation_uncertain {
                    ClaudeConversationState::CreationUncertain {
                        id: session_id.clone(),
                        message:
                            "the previous Claude session creation did not settle before shutdown"
                                .to_owned(),
                    }
                } else {
                    ClaudeConversationState::Ready {
                        id: session_id.clone(),
                    }
                };
                self.claude.resolved_model = session.resolved_model.clone();
                self.replace_transcript(claude_history(&session));
                self.turn = TurnState::Idle;
                self.thinking.clear_content();
                self.close_conversation_popup();
                self.reset_context_window();
                self.preferences
                    .set_auto_resume_claude_session(Some(session_id));
                self.notice = Some(if automatic_creation_uncertain {
                    "Claude session creation is uncertain after restart; use /resume or /new"
                        .to_owned()
                } else {
                    "Resumed the selected Claude conversation".to_owned()
                });
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ClaudeSessionSwitchFailed {
                session_id,
                message,
            } => {
                if let Some(picker) = self.conversation_popup_mut() {
                    if matches!(
                        &picker.phase,
                        ThreadPickerPhase::Resuming {
                            provider: ProviderId::Claude,
                            id,
                        } if id == session_id.as_str()
                    ) {
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = Some(format!(
                            "Could not resume the selected Claude conversation; the active conversation was preserved: {message}"
                        ));
                    }
                }
            }
            DomainEvent::ClaudeResumeFailed {
                session_id,
                message,
            } => {
                if self.active_provider == ProviderId::Claude
                    && self.preferences.claude.auto_resume_session_id.as_ref() == Some(&session_id)
                {
                    self.claude.conversation = ClaudeConversationState::ResumeFailed {
                        id: session_id,
                        message: message.clone(),
                    };
                    self.notice = Some(format!(
                        "Could not resume the saved Claude conversation; use /resume or /new: {message}"
                    ));
                }
            }
            DomainEvent::ClaudeTurnStarted {
                session_id,
                turn_id,
            } => {
                if self.active_provider != ProviderId::Claude
                    || !matches!(self.turn, TurnState::Starting)
                {
                    return Vec::new();
                }
                self.pending_new_claude_session = false;
                self.claude.conversation = ClaudeConversationState::Ready {
                    id: session_id.clone(),
                };
                if let Some(entry) = self.transcript.iter_mut().rev().find(|entry| {
                    entry.provider == ProviderId::Claude
                        && entry.role == TranscriptRole::User
                        && entry.turn_id.is_none()
                }) {
                    entry.turn_id = Some(turn_id.as_str().to_owned());
                }
                self.turn = TurnState::ClaudeStreaming {
                    session_id: session_id.clone(),
                    turn_id,
                };
                self.preferences
                    .set_auto_resume_claude_session(Some(session_id));
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ClaudeInitialized {
                session_id,
                turn_id,
                model,
            } => {
                if self.matches_claude_turn(&session_id, &turn_id) {
                    self.claude.resolved_model = Some(model);
                }
            }
            DomainEvent::ClaudeDelta {
                session_id,
                turn_id,
                delta,
            } => {
                if self.matches_claude_turn(&session_id, &turn_id) {
                    self.append_delta(turn_id.as_str(), CLAUDE_ASSISTANT_ITEM_ID, &delta);
                }
            }
            DomainEvent::ClaudeTurnFinished {
                session_id,
                turn_id,
                outcome,
                assistant_text,
                incomplete_assistant_text,
                creation_uncertain,
                failure,
            } => {
                if !self.matches_claude_turn(&session_id, &turn_id) {
                    return Vec::new();
                }
                let completion_persistence_failed =
                    outcome == ClaudeTurnOutcome::Completed && failure.is_some();
                let effective_outcome = if completion_persistence_failed {
                    ClaudeTurnOutcome::Failed
                } else {
                    outcome
                };
                if effective_outcome == ClaudeTurnOutcome::Completed {
                    if let Some(text) = assistant_text.as_deref() {
                        if let Err(message) =
                            self.reconcile_final(turn_id.as_str(), CLAUDE_ASSISTANT_ITEM_ID, text)
                        {
                            self.turn = TurnState::Failed {
                                turn_id: Some(turn_id.as_str().to_owned()),
                                message: message.clone(),
                            };
                            self.notice = Some(message);
                            return Vec::new();
                        }
                    }
                }
                if effective_outcome == ClaudeTurnOutcome::Failed {
                    let failed_text = if completion_persistence_failed {
                        assistant_text
                            .as_deref()
                            .or(incomplete_assistant_text.as_deref())
                    } else {
                        incomplete_assistant_text.as_deref()
                    };
                    if let Some(text) = failed_text {
                        if self
                            .reconcile_final(turn_id.as_str(), CLAUDE_ASSISTANT_ITEM_ID, text)
                            .is_ok()
                        {
                            let _ = self
                                .mark_failed_incomplete(turn_id.as_str(), CLAUDE_ASSISTANT_ITEM_ID);
                        } else {
                            self.notice =
                                Some("Claude final response contradicted streamed text".to_owned());
                        }
                    }
                }
                let uncertain_message = creation_uncertain.then(|| {
                    failure.as_ref().map_or_else(
                        || "Claude session creation did not become established".to_owned(),
                        |error| {
                            format!(
                                "Claude session creation did not become established ({:?}/{:?})",
                                error.stage, error.category
                            )
                        },
                    )
                });
                self.turn = match effective_outcome {
                    ClaudeTurnOutcome::Completed => TurnState::Completed {
                        turn_id: turn_id.as_str().to_owned(),
                    },
                    ClaudeTurnOutcome::Interrupted => TurnState::Interrupted {
                        turn_id: turn_id.as_str().to_owned(),
                    },
                    ClaudeTurnOutcome::Failed | ClaudeTurnOutcome::InProgress => {
                        let message = failure.map_or_else(
                            || "Claude turn failed".to_owned(),
                            |error| {
                                format!(
                                    "Claude turn failed ({:?}/{:?})",
                                    error.stage, error.category
                                )
                            },
                        );
                        TurnState::Failed {
                            turn_id: Some(turn_id.as_str().to_owned()),
                            message,
                        }
                    }
                };
                if let Some(message) = uncertain_message {
                    self.claude.conversation = ClaudeConversationState::CreationUncertain {
                        id: session_id.clone(),
                        message: message.clone(),
                    };
                    self.preferences
                        .set_auto_resume_claude_session(Some(session_id));
                    self.notice = Some(format!(
                        "{message}; use /resume or /new before sending again"
                    ));
                    return vec![Effect::Persist(self.preferences.clone())];
                }
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }

    pub(in crate::app) fn matches_claude_turn(
        &self,
        session_id: &ClaudeSessionId,
        turn_id: &ClaudeTurnId,
    ) -> bool {
        matches!(
            &self.turn,
            TurnState::ClaudeStreaming {
                session_id: expected_session,
                turn_id: expected_turn,
            } if expected_session == session_id && expected_turn == turn_id
        )
    }
}

fn claude_history(session: &ClaudeSessionV1) -> Vec<TranscriptEntry> {
    let mut history = Vec::new();
    for turn in &session.turns {
        history.push(TranscriptEntry {
            provider: ProviderId::Claude,
            role: TranscriptRole::User,
            status: TranscriptEntryStatus::Normal,
            text: turn.user_text.clone(),
            item_id: None,
            turn_id: Some(turn.id.as_str().to_owned()),
        });
        let assistant = match turn.outcome {
            ClaudeTurnOutcome::Completed => turn
                .assistant_text
                .as_ref()
                .map(|text| (text, TranscriptEntryStatus::Normal)),
            ClaudeTurnOutcome::Failed => turn
                .incomplete_assistant_text
                .as_ref()
                .map(|text| (text, TranscriptEntryStatus::FailedIncomplete)),
            ClaudeTurnOutcome::InProgress | ClaudeTurnOutcome::Interrupted => None,
        };
        if let Some((text, status)) = assistant {
            history.push(TranscriptEntry {
                provider: ProviderId::Claude,
                role: TranscriptRole::Assistant,
                status,
                text: text.clone(),
                item_id: Some(CLAUDE_ASSISTANT_ITEM_ID.to_owned()),
                turn_id: Some(turn.id.as_str().to_owned()),
            });
        }
    }
    history
}
