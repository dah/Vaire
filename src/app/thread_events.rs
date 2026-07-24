use super::*;

impl AppState {
    pub(in crate::app) fn reduce_thread_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::ResumeStarted { id } => {
                // Account loading already moves the reducer into `Resuming` before it emits the
                // backend effect. Treat this echo as acknowledgement only, so a delayed event can
                // never detach a newer active thread.
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
            }
            DomainEvent::ResumeSucceeded { id, history } => {
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
                self.reset_context_window();
                self.thread = ThreadState::Ready { id: id.clone() };
                self.turn = TurnState::Idle;
                self.replace_transcript(history);
                self.thinking.clear_content();
                self.preferences.set_auto_resume_thread(Some(id.clone()));
                self.register_thread_scope(&id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ResumeFailed { id, message } => {
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
                self.thread = ThreadState::ResumeFailed {
                    id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::NewThreadSucceeded { id } => {
                let Some(requested_scope) = self.pending_new_thread_scope.take() else {
                    return Vec::new();
                };
                if !matches!(&self.auth, AuthState::SignedIn { scope } if scope == &requested_scope)
                {
                    return Vec::new();
                }
                self.reset_context_window();
                self.thread = ThreadState::Ready { id: id.clone() };
                self.turn = TurnState::Idle;
                self.clear_transcript();
                self.close_conversation_popup();
                self.pending_thread_deletions = None;
                self.thinking.clear_content();
                self.preferences.set_auto_resume_thread(Some(id.clone()));
                if let AuthState::SignedIn { scope } = &self.auth {
                    self.preferences.codex.account_scope = scope.clone();
                }
                self.register_thread_scope(&id);
                self.notice = Some("Started a new thread".to_owned());
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::NewThreadFailed(message) => {
                if self.pending_new_thread_scope.take().is_none() {
                    return Vec::new();
                }
                self.notice = Some(format!(
                    "Could not start a new thread; the current thread was preserved: {message}"
                ));
            }
            DomainEvent::ThreadListLoaded(threads) => {
                let active_id = self.active_saved_thread_id().map(str::to_owned);
                let found_local_threads = !threads.is_empty();
                let threads = match &self.auth {
                    AuthState::SignedIn { scope: Some(scope) } => threads
                        .into_iter()
                        .filter(|thread| {
                            thread.provider != ProviderId::Codex
                                || self.preferences.codex.thread_account_scopes.get(&thread.id)
                                    == Some(scope)
                        })
                        .collect(),
                    _ => threads
                        .into_iter()
                        .filter(|thread| thread.provider != ProviderId::Codex)
                        .collect(),
                };
                if let Some(picker) = self.conversation_popup_mut() {
                    if matches!(picker.phase, ThreadPickerPhase::Loading) {
                        picker.threads = threads;
                        picker.selected = active_id
                            .as_deref()
                            .and_then(|active| {
                                picker.threads.iter().position(|thread| thread.id == active)
                            })
                            .unwrap_or(0);
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = picker.threads.is_empty().then(|| {
                            if found_local_threads {
                                "No saved threads are registered to this ChatGPT account".to_owned()
                            } else {
                                "No saved Vairë threads were found".to_owned()
                            }
                        });
                    }
                }
            }
            DomainEvent::ThreadListFailed(message) => {
                if let Some(picker) = self.conversation_popup_mut() {
                    if matches!(picker.phase, ThreadPickerPhase::Loading) {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.message = Some(format!("Could not load threads: {message}"));
                    }
                }
            }
            DomainEvent::ThreadSwitchSucceeded {
                id,
                history,
                model,
                reasoning,
            } => {
                let matches_request = self.conversation_popup().is_some_and(|picker| {
                    matches!(
                        &picker.phase,
                        ThreadPickerPhase::Resuming {
                            provider: ProviderId::Codex,
                            id: expected,
                        } if expected == &id
                    )
                });
                if matches_request {
                    if !self.commit_provider_selection(ProviderId::Codex, model, Some(reasoning)) {
                        if let Some(picker) = self.conversation_popup_mut() {
                            picker.phase = ThreadPickerPhase::Ready;
                            picker.message = Some(
                                "The selected Codex model is no longer available; the active conversation was preserved"
                                    .to_owned(),
                            );
                        }
                        return Vec::new();
                    }
                    self.openrouter.conversation = OpenRouterConversationState::None;
                    self.reset_context_window();
                    self.thread = ThreadState::Ready { id: id.clone() };
                    self.turn = TurnState::Idle;
                    self.replace_transcript(history);
                    self.close_conversation_popup();
                    self.thinking.clear_content();
                    self.preferences.set_auto_resume_thread(Some(id.clone()));
                    if let AuthState::SignedIn { scope } = &self.auth {
                        self.preferences.codex.account_scope = scope.clone();
                    }
                    self.register_thread_scope(&id);
                    self.notice = Some("Resumed the selected thread".to_owned());
                    return vec![Effect::Persist(self.preferences.clone())];
                }
            }
            DomainEvent::ThreadSwitchFailed { id, message } => {
                if let Some(picker) = self.conversation_popup_mut() {
                    if matches!(
                        &picker.phase,
                        ThreadPickerPhase::Resuming {
                            provider: ProviderId::Codex,
                            id: expected,
                        } if expected == &id
                    ) {
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = Some(format!(
                            "Could not resume the selected thread; the active thread was preserved: {message}"
                        ));
                    }
                }
            }
            DomainEvent::ThreadDeletionFinished {
                requested,
                deleted,
                failures,
            } => {
                let Some(expected_ids) = self.pending_thread_deletions.take() else {
                    return Vec::new();
                };
                let phase_request_count =
                    match self.conversation_popup().map(|picker| &picker.phase) {
                        Some(ThreadPickerPhase::Deleting { requested }) => *requested,
                        _ => return Vec::new(),
                    };
                let mut reported_ids = BTreeSet::new();
                let no_duplicate_results = deleted
                    .iter()
                    .chain(failures.iter().map(|failure| &failure.id))
                    .all(|id| reported_ids.insert(id.clone()));
                let result_count_matches = deleted
                    .len()
                    .checked_add(failures.len())
                    .is_some_and(|count| count == requested);
                let result_matches_request = phase_request_count == requested
                    && requested == expected_ids.len()
                    && result_count_matches
                    && no_duplicate_results
                    && reported_ids == expected_ids;
                if !result_matches_request {
                    if let Some(picker) = self.conversation_popup_mut() {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.confirmation = None;
                        picker.message = Some(
                            "Thread deletion result did not match the requested scope".to_owned(),
                        );
                    }
                    return Vec::new();
                }

                let active_id = self.active_saved_thread_id().map(str::to_owned);
                if let Some(picker) = self.conversation_popup_mut() {
                    let protected_reported = deleted
                        .iter()
                        .any(|id| active_id.as_deref() == Some(id.as_str()));
                    let safe_deleted = deleted
                        .iter()
                        .filter(|id| active_id.as_deref() != Some(id.as_str()))
                        .cloned()
                        .collect::<std::collections::HashSet<_>>();
                    picker
                        .threads
                        .retain(|thread| !safe_deleted.contains(thread.id.as_str()));
                    picker.selected = picker.selected.min(picker.threads.len().saturating_sub(1));
                    picker.phase = ThreadPickerPhase::Ready;
                    picker.confirmation = None;

                    let deleted_count = safe_deleted.len();
                    let mut message = if failures.is_empty() && !protected_reported {
                        format!("Deleted {deleted_count} inactive thread(s)")
                    } else {
                        format!(
                            "Deleted {deleted_count} of {requested} inactive thread(s); {} failed",
                            failures.len() + usize::from(protected_reported)
                        )
                    };
                    if !failures.is_empty() {
                        let details = failures
                            .iter()
                            .map(|failure| format!("{}: {}", failure.id, failure.message))
                            .collect::<Vec<_>>()
                            .join("; ");
                        message.push_str(&format!(" — {details}"));
                    }
                    if protected_reported {
                        message
                            .push_str(" — ignored an invalid result for the active saved thread");
                    }
                    picker.message = Some(message);
                    let mut removed_scope = false;
                    for id in &safe_deleted {
                        removed_scope |= self
                            .preferences
                            .codex
                            .thread_account_scopes
                            .remove(id)
                            .is_some();
                    }
                    if removed_scope {
                        return vec![Effect::Persist(self.preferences.clone())];
                    }
                }
            }
            DomainEvent::ThreadStarted { id } => {
                if !matches!(self.thread, ThreadState::None)
                    || !matches!(self.turn, TurnState::Starting)
                {
                    return Vec::new();
                }
                self.reset_context_window();
                self.thread = ThreadState::Ready { id: id.clone() };
                self.preferences.set_auto_resume_thread(Some(id.clone()));
                if let AuthState::SignedIn { scope } = &self.auth {
                    self.preferences.codex.account_scope = scope.clone();
                }
                self.register_thread_scope(&id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }
}
