use super::*;

impl OpenRouterService {
    pub async fn logout(
        &mut self,
    ) -> (
        Vec<OpenRouterServiceEvent>,
        Result<(), crate::credentials::CredentialStoreError>,
    ) {
        if let Some(cancel) = self.control_cancel.take() {
            cancel.cancel();
        }
        self.join_control_draining().await;
        self.interrupt_turn();
        let drained = self.join_chat_draining().await;
        let credentials = self.credentials.clone();
        let result = tokio::task::spawn_blocking(move || {
            credentials.delete_with_commit(CredentialAccount::OpenRouterApiKey)
        })
        .await
        .map_err(|_| {
            crate::credentials::CredentialStoreError::new(
                crate::credentials::CredentialFailureCategory::Delete,
            )
        })
        .and_then(|result| result)
        .map(|_| ());
        (drained, result)
    }

    pub async fn next_event(&mut self) -> Option<OpenRouterServiceEvent> {
        let event = if self.prefer_chat_event {
            tokio::select! {
                biased;
                event = self.chat_events_rx.recv() => event,
                event = self.control_events_rx.recv() => event,
            }
        } else {
            tokio::select! {
                biased;
                event = self.control_events_rx.recv() => event,
                event = self.chat_events_rx.recv() => event,
            }
        };
        self.prefer_chat_event = !self.prefer_chat_event;
        event
    }

    pub async fn shutdown(&mut self) -> Vec<OpenRouterServiceEvent> {
        if let Some(cancel) = self.control_cancel.take() {
            cancel.cancel();
        }
        self.interrupt_turn();
        self.join_control_draining().await;
        self.join_chat_draining().await
    }

    pub(super) async fn join_control_draining(&mut self) {
        if let Some(mut task) = self.control_task.take() {
            loop {
                tokio::select! {
                    _ = &mut task => break,
                    _ = self.control_events_rx.recv() => {}
                }
            }
        }
        self.control_cancel = None;
    }

    pub(super) async fn join_chat_draining(&mut self) -> Vec<OpenRouterServiceEvent> {
        let mut drained = Vec::new();
        if let Some(mut task) = self.chat_task.take() {
            loop {
                tokio::select! {
                    biased;
                    event = self.chat_events_rx.recv() => {
                        if let Some(event) = event {
                            drained.push(event);
                        }
                    }
                    _ = &mut task => break,
                }
            }
        }
        while let Ok(event) = self.chat_events_rx.try_recv() {
            drained.push(event);
        }
        self.chat_cancel = None;
        drained
    }

    pub(super) fn reap_control(&mut self) {
        if self
            .control_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            self.control_task = None;
            self.control_cancel = None;
        }
    }

    pub(super) fn reap_chat(&mut self) {
        if self.chat_task.as_ref().is_some_and(JoinHandle::is_finished) {
            self.chat_task = None;
            self.chat_cancel = None;
        }
    }
}
