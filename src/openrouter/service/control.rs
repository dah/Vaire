use super::*;

impl OpenRouterService {
    pub async fn startup(&self) -> OpenRouterServiceStart {
        let store = self.store.clone();
        let catalog = tokio::task::spawn_blocking(move || store.load_catalog())
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .map(|(_, catalog)| catalog)
            .unwrap_or_default();
        let credentials = self.credentials.clone();
        let configured = tokio::task::spawn_blocking(move || {
            credentials.load(CredentialAccount::OpenRouterApiKey)
        })
        .await;
        let auth = match configured {
            Ok(Ok(Some(_))) => OpenRouterAuthStatus::Unverified,
            Ok(Ok(None)) => OpenRouterAuthStatus::Missing,
            Ok(Err(_)) | Err(_) => OpenRouterAuthStatus::CredentialUnavailable,
        };
        OpenRouterServiceStart { auth, catalog }
    }

    pub fn revalidate_and_refresh(&mut self) -> Result<u64, &'static str> {
        self.start_control_task(None)
    }

    /// Validates before replacing the durable credential. The old credential is untouched on
    /// validation failure; after replacement, catalog refresh uses the newly stored value.
    pub fn replace_candidate(&mut self, candidate: SecretValue) -> Result<u64, SecretValue> {
        if self
            .chat_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(candidate);
        }
        if self
            .control_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err(candidate);
        }
        self.reap_control();
        let operation_id = self.next_control_operation_id;
        let Some(next_operation_id) = operation_id.checked_add(1) else {
            return Err(candidate);
        };
        self.next_control_operation_id = next_operation_id;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let store = self.store.clone();
        let events = self.control_events_tx.clone();
        self.control_task = Some(tokio::spawn(async move {
            let result = client
                .validate_candidate(&candidate, task_cancel.clone())
                .await;
            if let Err(error) = result {
                let _ = events
                    .send(OpenRouterServiceEvent::LoginFailed {
                        operation_id,
                        category: error.category(),
                    })
                    .await;
                return;
            }
            let replaced = tokio::task::spawn_blocking(move || {
                credentials.replace_with_commit(CredentialAccount::OpenRouterApiKey, candidate)
            })
            .await;
            if !matches!(replaced, Ok(Ok(_))) {
                let _ = events
                    .send(OpenRouterServiceEvent::LoginFailed {
                        operation_id,
                        category: OpenRouterFailureCategory::CredentialStore,
                    })
                    .await;
                return;
            }
            let _ = events
                .send(OpenRouterServiceEvent::AuthValidated { operation_id })
                .await;
            match client.fetch_catalog(task_cancel).await {
                Ok(catalog) => {
                    let saved_catalog = catalog.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        store.save_catalog_with_commit(now_ms(), &saved_catalog)
                    })
                    .await;
                    if !matches!(saved, Ok(Ok(_))) {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogFailed {
                                operation_id,
                                category: OpenRouterFailureCategory::CredentialStore,
                            })
                            .await;
                        return;
                    }
                    let _ = events
                        .send(OpenRouterServiceEvent::LoginSucceeded {
                            operation_id,
                            catalog,
                        })
                        .await;
                }
                Err(error) => {
                    let _ = events
                        .send(OpenRouterServiceEvent::CatalogFailed {
                            operation_id,
                            category: error.category(),
                        })
                        .await;
                }
            }
        }));
        self.control_cancel = Some(cancel);
        Ok(operation_id)
    }

    fn start_control_task(&mut self, _unused: Option<SecretValue>) -> Result<u64, &'static str> {
        if self
            .control_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Err("an OpenRouter authentication operation is already active");
        }
        self.reap_control();
        let operation_id = self.next_control_operation_id;
        self.next_control_operation_id = operation_id
            .checked_add(1)
            .ok_or("OpenRouter authentication operation IDs are exhausted")?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let client = self.client.clone();
        let store = self.store.clone();
        let events = self.control_events_tx.clone();
        self.control_task = Some(tokio::spawn(async move {
            if let Err(error) = client.validate_stored_key(task_cancel.clone()).await {
                let _ = events
                    .send(OpenRouterServiceEvent::CatalogFailed {
                        operation_id,
                        category: error.category(),
                    })
                    .await;
                return;
            }
            let _ = events
                .send(OpenRouterServiceEvent::AuthValidated { operation_id })
                .await;
            match client.fetch_catalog(task_cancel).await {
                Ok(catalog) => {
                    let saved_catalog = catalog.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        store.save_catalog_with_commit(now_ms(), &saved_catalog)
                    })
                    .await;
                    if matches!(saved, Ok(Ok(_))) {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogLoaded {
                                operation_id,
                                catalog,
                            })
                            .await;
                    } else {
                        let _ = events
                            .send(OpenRouterServiceEvent::CatalogFailed {
                                operation_id,
                                category: OpenRouterFailureCategory::CredentialStore,
                            })
                            .await;
                    }
                }
                Err(error) => {
                    let _ = events
                        .send(OpenRouterServiceEvent::CatalogFailed {
                            operation_id,
                            category: error.category(),
                        })
                        .await;
                }
            }
        }));
        self.control_cancel = Some(cancel);
        Ok(operation_id)
    }
}
