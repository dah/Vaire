use super::*;

impl AppState {
    pub(in crate::app) fn reduce_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            event @ (DomainEvent::PreferencesLoaded(_)
            | DomainEvent::Connecting
            | DomainEvent::Connected { .. }
            | DomainEvent::ConnectionFailed(_)
            | DomainEvent::ProcessExited(_)
            | DomainEvent::AccountLoaded(_)
            | DomainEvent::UnsupportedAccount(_)
            | DomainEvent::LoginStarted { .. }
            | DomainEvent::LoginFailed(_)
            | DomainEvent::LoggedOut
            | DomainEvent::CatalogLoaded(_)) => self.reduce_account_event(event),
            event @ (DomainEvent::OpenRouterStartup { .. }
            | DomainEvent::OpenRouterAuthChanged(_)
            | DomainEvent::OpenRouterCatalogLoaded(_)
            | DomainEvent::OpenRouterCatalogLoadedForAutomaticResume(_)
            | DomainEvent::OpenRouterOperationFailed(_)
            | DomainEvent::OpenRouterCandidateRejected(_)
            | DomainEvent::OpenRouterConversationStarted { .. }
            | DomainEvent::OpenRouterConversationRestored { .. }
            | DomainEvent::OpenRouterConversationSwitchFailed { .. }
            | DomainEvent::OpenRouterResumeFailed { .. }
            | DomainEvent::OpenRouterTurnStarted { .. }
            | DomainEvent::OpenRouterDelta { .. }
            | DomainEvent::OpenRouterUsage { .. }
            | DomainEvent::OpenRouterTurnFinished { .. }) => self.reduce_openrouter_event(event),
            event @ (DomainEvent::ResumeStarted { .. }
            | DomainEvent::ResumeSucceeded { .. }
            | DomainEvent::ResumeFailed { .. }
            | DomainEvent::NewThreadSucceeded { .. }
            | DomainEvent::NewThreadFailed(_)
            | DomainEvent::ThreadListLoaded(_)
            | DomainEvent::ThreadListFailed(_)
            | DomainEvent::ThreadSwitchSucceeded { .. }
            | DomainEvent::ThreadSwitchFailed { .. }
            | DomainEvent::ThreadDeletionFinished { .. }
            | DomainEvent::ThreadStarted { .. }) => self.reduce_thread_event(event),
            event => self.reduce_turn_event(event),
        }
    }

    fn reduce_openrouter_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::OpenRouterStartup { auth, catalog } => {
                self.openrouter.auth = auth;
                self.openrouter.catalog = catalog;
                if self.active_provider == ProviderId::OpenRouter {
                    self.selected_model = self
                        .preferences
                        .openrouter
                        .selected_model_id
                        .clone()
                        .and_then(|id| ModelKey::openrouter(id).ok());
                    self.selected_reasoning = None;
                    if self
                        .preferences
                        .openrouter
                        .auto_resume_conversation_id
                        .is_none()
                    {
                        self.validate_openrouter_selection();
                    }
                }
            }
            DomainEvent::OpenRouterAuthChanged(auth) => {
                self.openrouter.auth = auth;
                if auth != crate::openrouter::OpenRouterAuthStatus::Valid {
                    self.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
                }
                if auth == crate::openrouter::OpenRouterAuthStatus::Missing {
                    self.popup = None;
                }
                self.notice = Some(match auth {
                    crate::openrouter::OpenRouterAuthStatus::Valid => {
                        "OpenRouter credential is valid".to_owned()
                    }
                    crate::openrouter::OpenRouterAuthStatus::Missing => {
                        "OpenRouter is signed out".to_owned()
                    }
                    crate::openrouter::OpenRouterAuthStatus::Invalid => {
                        "OpenRouter credential is invalid; replace it with /login".to_owned()
                    }
                    crate::openrouter::OpenRouterAuthStatus::Unverified => {
                        "OpenRouter credential could not be verified".to_owned()
                    }
                    crate::openrouter::OpenRouterAuthStatus::CredentialUnavailable => {
                        "OpenRouter credential storage is unavailable".to_owned()
                    }
                });
            }
            DomainEvent::OpenRouterCatalogLoadedForAutomaticResume(catalog) => {
                self.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
                self.openrouter.auth = crate::openrouter::OpenRouterAuthStatus::Valid;
                self.openrouter.catalog = catalog;
                if matches!(self.popup, Some(PopupState::OpenRouterSecret)) {
                    self.popup = None;
                }
            }
            DomainEvent::OpenRouterCatalogLoaded(catalog) => {
                self.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
                let previous_model = self.selected_model.clone();
                self.openrouter.auth = crate::openrouter::OpenRouterAuthStatus::Valid;
                self.openrouter.catalog = catalog;
                if matches!(self.popup, Some(PopupState::OpenRouterSecret)) {
                    self.popup = None;
                    self.notice = Some("OpenRouter credential saved and verified".to_owned());
                }
                if self.active_provider == ProviderId::OpenRouter {
                    self.validate_openrouter_selection();
                    if previous_model != self.selected_model {
                        self.reset_context_window();
                        return vec![Effect::Persist(self.preferences.clone())];
                    }
                }
            }
            DomainEvent::OpenRouterOperationFailed(category) => {
                use crate::openrouter::OpenRouterFailureCategory;
                let candidate_was_saved = matches!(
                    self.openrouter.credential_validation,
                    OpenRouterCredentialValidation::Validating {
                        candidate_saved: true,
                        ..
                    }
                ) && self.openrouter.auth
                    == crate::openrouter::OpenRouterAuthStatus::Valid;
                self.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
                if candidate_was_saved && category == OpenRouterFailureCategory::Unauthorized {
                    self.openrouter.auth = crate::openrouter::OpenRouterAuthStatus::Invalid;
                    if matches!(self.popup, Some(PopupState::OpenRouterSecret)) {
                        self.popup = None;
                    }
                    self.notice = Some(
                        "OpenRouter saved the credential, but the provider rejected it during model refresh; replace it with /login"
                            .to_owned(),
                    );
                } else if candidate_was_saved {
                    if matches!(self.popup, Some(PopupState::OpenRouterSecret)) {
                        self.popup = None;
                    }
                    self.notice = Some(format!(
                        "OpenRouter credential was saved, but catalog refresh failed ({category:?})"
                    ));
                } else {
                    self.openrouter.auth = match category {
                        OpenRouterFailureCategory::Unauthorized => {
                            crate::openrouter::OpenRouterAuthStatus::Invalid
                        }
                        OpenRouterFailureCategory::MissingCredential => {
                            crate::openrouter::OpenRouterAuthStatus::Missing
                        }
                        OpenRouterFailureCategory::CredentialStore => {
                            crate::openrouter::OpenRouterAuthStatus::CredentialUnavailable
                        }
                        _ => self.openrouter.auth,
                    };
                    self.notice = Some(format!("OpenRouter operation failed ({category:?})"));
                }
            }
            DomainEvent::OpenRouterCandidateRejected(category) => {
                self.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
                self.notice = Some(format!(
                    "OpenRouter credential was not replaced ({category:?}); the existing credential was preserved"
                ));
            }
            DomainEvent::OpenRouterConversationStarted { conversation_id } => {
                if self.active_provider != ProviderId::OpenRouter || self.turn.is_active() {
                    return Vec::new();
                }
                self.openrouter.conversation = OpenRouterConversationState::Ready {
                    id: conversation_id.clone(),
                };
                self.clear_transcript();
                self.thinking.clear_content();
                self.reset_context_window();
                self.preferences
                    .set_auto_resume_conversation(Some(conversation_id));
                self.notice = Some("Started a new OpenRouter conversation".to_owned());
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::OpenRouterConversationRestored {
                conversation_id,
                history,
                model,
                automatic,
            } => {
                let picker_requested = !automatic
                    && self.conversation_popup().is_some_and(|picker| {
                        matches!(
                            &picker.phase,
                            ThreadPickerPhase::Resuming {
                                provider: ProviderId::OpenRouter,
                                id,
                            } if id == conversation_id.as_str()
                        )
                    });
                let automatic_requested = automatic
                    && self.active_provider == ProviderId::OpenRouter
                    && self
                        .preferences
                        .openrouter
                        .auto_resume_conversation_id
                        .as_ref()
                        == Some(&conversation_id)
                    && !matches!(
                        &self.openrouter.conversation,
                        OpenRouterConversationState::Ready { .. }
                    );
                if !picker_requested && !automatic_requested {
                    return Vec::new();
                }
                if !self.commit_provider_selection(ProviderId::OpenRouter, model, None) {
                    if picker_requested {
                        if let Some(picker) = self.conversation_popup_mut() {
                            picker.phase = ThreadPickerPhase::Ready;
                            picker.message = Some(
                                "The selected OpenRouter model is no longer available; the active conversation was preserved"
                                    .to_owned(),
                            );
                        }
                    } else {
                        self.openrouter.conversation = OpenRouterConversationState::ResumeFailed {
                            id: conversation_id,
                            message: "saved OpenRouter model is unavailable".to_owned(),
                        };
                    }
                    return Vec::new();
                }
                self.thread = ThreadState::None;
                self.openrouter.conversation = OpenRouterConversationState::Ready {
                    id: conversation_id.clone(),
                };
                self.replace_transcript(history);
                self.turn = TurnState::Idle;
                self.thinking.clear_content();
                self.close_conversation_popup();
                self.reset_context_window();
                self.preferences
                    .set_auto_resume_conversation(Some(conversation_id));
                self.notice = Some("Resumed the selected OpenRouter conversation".to_owned());
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::OpenRouterConversationSwitchFailed {
                conversation_id,
                message,
            } => {
                if let Some(picker) = self.conversation_popup_mut() {
                    if matches!(
                        &picker.phase,
                        ThreadPickerPhase::Resuming {
                            provider: ProviderId::OpenRouter,
                            id,
                        } if id == conversation_id.as_str()
                    ) {
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = Some(format!(
                            "Could not resume the selected conversation; the active conversation was preserved: {message}"
                        ));
                    }
                }
            }
            DomainEvent::OpenRouterResumeFailed { conversation_id } => {
                if self.active_provider == ProviderId::OpenRouter
                    && self
                        .preferences
                        .openrouter
                        .auto_resume_conversation_id
                        .as_ref()
                        == Some(&conversation_id)
                {
                    self.openrouter.conversation = OpenRouterConversationState::ResumeFailed {
                        id: conversation_id,
                        message: "saved OpenRouter conversation could not be restored".to_owned(),
                    };
                    self.notice = Some(
                        "Could not resume the saved OpenRouter conversation; use /resume or /new"
                            .to_owned(),
                    );
                }
            }
            DomainEvent::OpenRouterTurnStarted {
                conversation_id,
                turn_id,
            } => {
                if self.active_provider != ProviderId::OpenRouter
                    || !matches!(self.turn, TurnState::Starting)
                {
                    return Vec::new();
                }
                self.openrouter.conversation = OpenRouterConversationState::Ready {
                    id: conversation_id.clone(),
                };
                if let Some(entry) = self
                    .transcript
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.role == TranscriptRole::User && entry.turn_id.is_none())
                {
                    entry.turn_id = Some(turn_id.as_str().to_owned());
                }
                self.turn = TurnState::OpenRouterStreaming {
                    conversation_id: conversation_id.clone(),
                    turn_id,
                };
                self.preferences
                    .set_auto_resume_conversation(Some(conversation_id));
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::OpenRouterDelta {
                conversation_id,
                turn_id,
                delta,
            } => {
                if self.matches_openrouter_turn(&conversation_id, &turn_id) {
                    self.append_delta(turn_id.as_str(), "openrouter-assistant", &delta);
                }
            }
            DomainEvent::OpenRouterUsage {
                conversation_id,
                turn_id,
                usage,
            } => {
                if self.matches_openrouter_turn(&conversation_id, &turn_id) {
                    let window = self
                        .selected_model
                        .as_ref()
                        .filter(|key| key.provider == ProviderId::OpenRouter)
                        .and_then(|key| {
                            self.openrouter
                                .catalog
                                .iter()
                                .find(|model| model.id == key.id)
                        })
                        .and_then(|model| model.context_length)
                        .and_then(|value| i64::try_from(value).ok());
                    self.context_remaining_percent = i64::try_from(usage.total_tokens)
                        .ok()
                        .and_then(|tokens| remaining_context_percent(tokens, window));
                }
            }
            DomainEvent::OpenRouterTurnFinished {
                conversation_id,
                turn_id,
                outcome,
                assistant_text,
                incomplete_assistant_text,
                failure_stage,
            } => {
                if !self.matches_openrouter_turn(&conversation_id, &turn_id) {
                    return Vec::new();
                }
                let terminal_snapshot = match &outcome {
                    TurnOutcome::Completed => assistant_text.as_deref().map(|text| (text, false)),
                    TurnOutcome::Failed(_) => incomplete_assistant_text
                        .as_deref()
                        .map(|text| (text, true)),
                    TurnOutcome::Interrupted => None,
                };
                if let Some((text, failed_incomplete)) = terminal_snapshot {
                    if self
                        .reconcile_final(turn_id.as_str(), "openrouter-assistant", text)
                        .is_err()
                    {
                        self.notice =
                            Some("OpenRouter final response contradicted streamed text".to_owned());
                    } else if failed_incomplete {
                        let _ =
                            self.mark_failed_incomplete(turn_id.as_str(), "openrouter-assistant");
                    }
                }
                self.turn = match outcome {
                    TurnOutcome::Completed => TurnState::Completed {
                        turn_id: turn_id.as_str().to_owned(),
                    },
                    TurnOutcome::Interrupted => TurnState::Interrupted {
                        turn_id: turn_id.as_str().to_owned(),
                    },
                    TurnOutcome::Failed(message) => {
                        let message = if let Some(stage) = failure_stage {
                            format!("{message}; stream stage {stage:?}")
                        } else {
                            message
                        };
                        TurnState::Failed {
                            turn_id: Some(turn_id.as_str().to_owned()),
                            message,
                        }
                    }
                };
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }
}

impl AppState {
    fn matches_openrouter_turn(
        &self,
        conversation_id: &OpenRouterConversationId,
        turn_id: &OpenRouterTurnId,
    ) -> bool {
        matches!(
            &self.turn,
            TurnState::OpenRouterStreaming {
                conversation_id: active_conversation,
                turn_id: active_turn,
            } if active_conversation == conversation_id && active_turn == turn_id
        )
    }

    pub(in crate::app) fn validate_openrouter_selection(&mut self) {
        if self.active_provider != ProviderId::OpenRouter {
            return;
        }
        let resolved = self.resolve_provider_selection(ProviderId::OpenRouter);
        if let Some((model, _)) = resolved {
            let _ = self.commit_provider_selection(ProviderId::OpenRouter, model, None);
        } else {
            self.selected_model = None;
            self.selected_reasoning = None;
            self.preferences.openrouter.selected_model_id = None;
        }
    }
}
