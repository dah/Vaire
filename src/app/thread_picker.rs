use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadChoice {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadPickerPhase {
    Loading,
    Ready,
    Resuming { id: String },
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
        let Some(picker) = &mut self.thread_picker else {
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
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return Vec::new();
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return Vec::new();
        }
        let Some(selected) = picker.selected_thread().cloned() else {
            picker.message = Some("No thread is available to resume".to_owned());
            return Vec::new();
        };
        if matches!(&self.thread, ThreadState::Ready { id } if id == &selected.id)
            && active_id.as_deref() == Some(selected.id.as_str())
        {
            self.thread_picker = None;
            self.notice = Some("That thread is already active".to_owned());
            return Vec::new();
        }
        picker.phase = ThreadPickerPhase::Resuming {
            id: selected.id.clone(),
        };
        picker.message = Some(format!("Opening {}…", selected.title));
        vec![Effect::SwitchThread { id: selected.id }]
    }

    pub(in crate::app) fn close_thread_picker(&mut self) {
        let busy = self.thread_picker.as_ref().is_some_and(|picker| {
            matches!(
                picker.phase,
                ThreadPickerPhase::Resuming { .. } | ThreadPickerPhase::Deleting { .. }
            )
        });
        if busy {
            if let Some(picker) = &mut self.thread_picker {
                picker.message = Some("Wait for the current thread operation to finish".to_owned());
            }
        } else {
            self.thread_picker = None;
        }
    }

    pub(in crate::app) fn request_selected_thread_delete(&mut self) {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let Some(target) = picker.selected_thread().cloned() else {
            picker.message = Some("No thread is selected".to_owned());
            return;
        };
        if active_id.as_deref() == Some(target.id.as_str()) {
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
        let Some(picker) = &mut self.thread_picker else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let targets = picker
            .threads
            .iter()
            .filter(|thread| active_id.as_deref() != Some(thread.id.as_str()))
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
        let Some(picker) = &mut self.thread_picker else {
            return Vec::new();
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) {
            return Vec::new();
        }
        let Some(confirmation) = picker.confirmation.take() else {
            return Vec::new();
        };
        let targets = confirmation.targets();
        if targets
            .iter()
            .any(|target| active_id.as_deref() == Some(target.id.as_str()))
        {
            picker.message = Some(
                "Deletion cancelled because its scope included the active saved thread".to_owned(),
            );
            return Vec::new();
        }
        let ids = targets
            .into_iter()
            .map(|target| target.id)
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
        vec![Effect::DeleteThreads { ids }]
    }

    pub(in crate::app) fn active_saved_thread_id(&self) -> Option<&str> {
        self.preferences
            .thread_id
            .as_deref()
            .or(match &self.thread {
                ThreadState::Ready { id } => Some(id.as_str()),
                _ => None,
            })
    }

    pub(in crate::app) fn register_thread_scope(&mut self, thread_id: &str) {
        if let AuthState::SignedIn { scope: Some(scope) } = &self.auth {
            self.preferences
                .thread_account_scopes
                .insert(thread_id.to_owned(), scope.clone());
        }
    }
}
