use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadChoice {
    pub provider: ProviderId,
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadPickerPhase {
    Loading,
    Ready,
    Resuming { provider: ProviderId, id: String },
    Deleting { requested: usize },
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadDeleteConfirmation {
    Selected { target: ThreadChoice },
    AllInactive { targets: Vec<ThreadChoice> },
}

impl ThreadDeleteConfirmation {
    pub fn targets(&self) -> Vec<ThreadChoice> {
        match self {
            Self::Selected { target } => vec![target.clone()],
            Self::AllInactive { targets } => targets.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadPickerState {
    pub phase: ThreadPickerPhase,
    pub threads: Vec<ThreadChoice>,
    pub selected: usize,
    pub confirmation: Option<ThreadDeleteConfirmation>,
    pub message: Option<String>,
}

impl ThreadPickerState {
    pub(in crate::app) fn loading() -> Self {
        Self {
            phase: ThreadPickerPhase::Loading,
            threads: Vec::new(),
            selected: 0,
            confirmation: None,
            message: None,
        }
    }

    pub fn selected_thread(&self) -> Option<&ThreadChoice> {
        self.threads.get(self.selected)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadDeletionFailure {
    pub id: String,
    pub message: String,
}

impl AppState {
    pub(in crate::app) fn move_thread_picker(&mut self, delta: isize) {
        let Some(PopupState::Conversation(picker)) = &mut self.popup else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready)
            || picker.confirmation.is_some()
            || picker.threads.is_empty()
        {
            return;
        }
        picker.message = None;
        let last = picker.threads.len().saturating_sub(1);
        picker.selected = if delta < 0 {
            picker.selected.saturating_sub(1)
        } else {
            picker.selected.saturating_add(1).min(last)
        };
    }

    pub(in crate::app) fn select_thread_picker(&mut self) -> Vec<Effect> {
        let selected = match self.conversation_popup() {
            Some(picker)
                if matches!(picker.phase, ThreadPickerPhase::Ready)
                    && picker.confirmation.is_none() =>
            {
                picker.selected_thread().cloned()
            }
            _ => return Vec::new(),
        };
        let Some(selected) = selected else {
            if let Some(picker) = self.conversation_popup_mut() {
                picker.message = Some("No thread is available to resume".to_owned());
            }
            return Vec::new();
        };
        let already_active = match selected.provider {
            ProviderId::Codex => {
                matches!(&self.thread, ThreadState::Ready { id } if id == &selected.id)
                    && self.active_provider == ProviderId::Codex
            }
            ProviderId::OpenRouter => {
                matches!(
                    &self.openrouter.conversation,
                    OpenRouterConversationState::Ready { id } if id.as_str() == selected.id
                ) && self.active_provider == ProviderId::OpenRouter
            }
            ProviderId::Claude => {
                matches!(
                    &self.claude.conversation,
                    ClaudeConversationState::Ready { id } if id.as_str() == selected.id
                ) && self.active_provider == ProviderId::Claude
            }
        };
        if already_active {
            self.close_conversation_popup();
            self.notice = Some("That thread is already active".to_owned());
            return Vec::new();
        }

        if selected.provider == ProviderId::OpenRouter
            && self.openrouter.auth
                == crate::openrouter::OpenRouterAuthStatus::CredentialUnavailable
        {
            if let Some(picker) = self.conversation_popup_mut() {
                picker.message = Some(
                    "OpenRouter runtime or credential storage is unavailable; restart Vairë after fixing the local storage path"
                        .to_owned(),
                );
            }
            return Vec::new();
        }

        if selected.provider == ProviderId::Claude {
            if !matches!(self.claude.availability, ClaudeAvailability::Ready)
                || self.claude.auth != ClaudeAuthStatus::Subscription
            {
                if let Some(picker) = self.conversation_popup_mut() {
                    picker.message = Some(
                        "Claude Code must be available and signed in to a subscription before resuming"
                            .to_owned(),
                    );
                }
                return Vec::new();
            }
            let Ok(id) = selected.id.parse() else {
                if let Some(picker) = self.conversation_popup_mut() {
                    picker.phase = ThreadPickerPhase::Failed;
                    picker.message = Some("Invalid Claude session identity".to_owned());
                }
                return Vec::new();
            };
            if let Some(picker) = self.conversation_popup_mut() {
                picker.phase = ThreadPickerPhase::Resuming {
                    provider: ProviderId::Claude,
                    id: selected.id,
                };
                picker.message = Some(format!("Opening {}…", selected.title));
            }
            return vec![Effect::SwitchClaudeSession { id }];
        }

        let Some((model, reasoning)) = self.resolve_provider_selection(selected.provider) else {
            if let Some(picker) = self.conversation_popup_mut() {
                picker.message = Some(format!(
                    "{} model catalog is not ready; refresh or select a model first",
                    selected.provider
                ));
            }
            return Vec::new();
        };
        if selected.provider == ProviderId::Codex
            && (!matches!(self.connection, ConnectionState::Ready { .. })
                || !matches!(self.auth, AuthState::SignedIn { scope: Some(_) }))
        {
            if let Some(picker) = self.conversation_popup_mut() {
                picker.message = Some(
                    "Codex must be connected with a known ChatGPT account before resuming"
                        .to_owned(),
                );
            }
            return Vec::new();
        }

        let effect = match selected.provider {
            ProviderId::Codex => Effect::SwitchThread {
                id: selected.id.clone(),
                model,
                reasoning: reasoning.expect("Codex selection always has reasoning"),
            },
            ProviderId::OpenRouter => match selected.id.parse() {
                Ok(id) => Effect::SwitchOpenRouterConversation { id, model },
                Err(_) => {
                    if let Some(picker) = self.conversation_popup_mut() {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.message =
                            Some("Invalid OpenRouter conversation identity".to_owned());
                    }
                    return Vec::new();
                }
            },
            ProviderId::Claude => {
                unreachable!("Claude resumes are handled before model resolution")
            }
        };
        if let Some(picker) = self.conversation_popup_mut() {
            picker.phase = ThreadPickerPhase::Resuming {
                provider: selected.provider,
                id: selected.id,
            };
            picker.message = Some(format!("Opening {}…", selected.title));
        }
        vec![effect]
    }

    pub(in crate::app) fn close_thread_picker(&mut self) {
        let busy = self.conversation_popup().is_some_and(|picker| {
            matches!(
                picker.phase,
                ThreadPickerPhase::Resuming { .. } | ThreadPickerPhase::Deleting { .. }
            )
        });
        if busy {
            if let Some(picker) = self.conversation_popup_mut() {
                picker.message = Some("Wait for the current thread operation to finish".to_owned());
            }
        } else {
            self.close_conversation_popup();
        }
    }

    pub(in crate::app) fn request_selected_thread_delete(&mut self) {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(PopupState::Conversation(picker)) = &mut self.popup else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let Some(target) = picker.selected_thread().cloned() else {
            picker.message = Some("No thread is selected".to_owned());
            return;
        };
        if target.provider == self.active_provider
            && active_id.as_deref() == Some(target.id.as_str())
        {
            picker.message = Some(
                "The active thread cannot be deleted. Switch threads or use /new first.".to_owned(),
            );
            return;
        }
        picker.message = None;
        picker.confirmation = Some(ThreadDeleteConfirmation::Selected { target });
    }

    pub(in crate::app) fn request_clear_inactive_threads(&mut self) {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(PopupState::Conversation(picker)) = &mut self.popup else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let targets = picker
            .threads
            .iter()
            .filter(|thread| {
                thread.provider != self.active_provider
                    || active_id.as_deref() != Some(thread.id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if targets.is_empty() {
            picker.message = Some("There are no inactive threads to delete".to_owned());
            return;
        }
        picker.message = None;
        picker.confirmation = Some(ThreadDeleteConfirmation::AllInactive { targets });
    }

    pub(in crate::app) fn confirm_thread_delete(&mut self) -> Vec<Effect> {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(PopupState::Conversation(picker)) = &mut self.popup else {
            return Vec::new();
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) {
            return Vec::new();
        }
        let Some(confirmation) = picker.confirmation.take() else {
            return Vec::new();
        };
        let targets = confirmation.targets();
        if targets.iter().any(|target| {
            target.provider == self.active_provider
                && active_id.as_deref() == Some(target.id.as_str())
        }) {
            picker.message = Some(
                "Deletion cancelled because its scope included the active saved thread".to_owned(),
            );
            return Vec::new();
        }
        let claude_ids = targets
            .iter()
            .filter(|target| target.provider == ProviderId::Claude)
            .filter_map(|target| target.id.parse().ok())
            .collect::<Vec<_>>();
        let openrouter_ids = targets
            .iter()
            .filter(|target| target.provider == ProviderId::OpenRouter)
            .filter_map(|target| target.id.parse().ok())
            .collect::<Vec<_>>();
        let ids = targets
            .iter()
            .map(|target| target.id.clone())
            .collect::<Vec<_>>();
        let expected_ids = ids.iter().cloned().collect::<BTreeSet<_>>();
        if expected_ids.len() != ids.len() {
            picker.phase = ThreadPickerPhase::Failed;
            picker.message =
                Some("Deletion cancelled because the thread list was invalid".to_owned());
            return Vec::new();
        }
        self.pending_thread_deletions = Some(expected_ids);
        picker.phase = ThreadPickerPhase::Deleting {
            requested: ids.len(),
        };
        picker.message = Some(format!("Deleting {} inactive thread(s)…", ids.len()));
        let codex_ids = targets
            .into_iter()
            .filter(|target| target.provider == ProviderId::Codex)
            .map(|target| target.id)
            .collect::<Vec<_>>();
        match (
            codex_ids.is_empty(),
            openrouter_ids.is_empty(),
            claude_ids.is_empty(),
        ) {
            (false, true, true) => vec![Effect::DeleteThreads { ids: codex_ids }],
            (true, false, true) => {
                vec![Effect::DeleteOpenRouterConversations {
                    ids: openrouter_ids,
                }]
            }
            (true, true, false) => vec![Effect::DeleteClaudeSessions { ids: claude_ids }],
            _ => vec![Effect::DeleteAllConversations {
                codex_ids,
                openrouter_ids,
                claude_ids,
            }],
        }
    }

    pub(crate) fn active_saved_thread_id(&self) -> Option<&str> {
        if self.active_provider == ProviderId::Claude {
            return self
                .preferences
                .claude
                .auto_resume_session_id
                .as_ref()
                .map(ClaudeSessionId::as_str)
                .or(match &self.claude.conversation {
                    ClaudeConversationState::Ready { id } => Some(id.as_str()),
                    _ => None,
                });
        }
        if self.active_provider == ProviderId::OpenRouter {
            return self
                .preferences
                .openrouter
                .auto_resume_conversation_id
                .as_ref()
                .map(OpenRouterConversationId::as_str)
                .or(match &self.openrouter.conversation {
                    OpenRouterConversationState::Ready { id } => Some(id.as_str()),
                    _ => None,
                });
        }
        self.preferences
            .codex
            .auto_resume_thread_id
            .as_deref()
            .or(match &self.thread {
                ThreadState::Ready { id } => Some(id.as_str()),
                _ => None,
            })
    }

    pub(in crate::app) fn register_thread_scope(&mut self, thread_id: &str) {
        if let AuthState::SignedIn { scope: Some(scope) } = &self.auth {
            self.preferences
                .codex
                .thread_account_scopes
                .insert(thread_id.to_owned(), scope.clone());
        }
    }
}
