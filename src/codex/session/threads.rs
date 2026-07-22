use super::*;

impl SessionService {
    pub async fn list_threads(&self) -> Result<Vec<ThreadListEntry>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_threads = HashSet::new();
        let mut threads = Vec::new();
        let mut pages = 0;
        let mut budget = PaginationBudget::default();
        loop {
            let response: ThreadListResponse = decode(
                "thread/list",
                self.transport
                    .request_default(
                        "thread/list",
                        ThreadListParams {
                            source_kinds: vec![
                                ThreadSourceKind::AppServer,
                                ThreadSourceKind::Vscode,
                            ],
                            archived: false,
                            cursor: cursor.clone(),
                            cwd: self.paths.conversation.clone(),
                            limit: 50,
                            sort_direction: "desc".to_owned(),
                            sort_key: "updated_at".to_owned(),
                        },
                    )
                    .await?,
            )?;
            pages += 1;
            validate_page_len("thread/list", response.data.len(), MAX_THREAD_PAGE_ITEMS)?;
            for thread in response.data {
                if !valid_identifier(&thread.id) {
                    return Err(SessionError::Protocol(
                        "thread/list returned an invalid thread id".to_owned(),
                    ));
                }
                if thread.cwd != self.paths.conversation {
                    return Err(SessionError::Protocol(
                        "thread/list returned a thread outside the AgentHarness working directory"
                            .to_owned(),
                    ));
                }
                if !thread.source.is_supported_resume_source() {
                    return Err(SessionError::Protocol(
                        "thread/list returned a thread from an unsupported source".to_owned(),
                    ));
                }
                if !thread.ephemeral && seen_threads.insert(thread.id.clone()) {
                    budget.retain("thread/list", thread_retained_bytes(&thread))?;
                    threads.push(thread);
                }
            }
            let Some(next) = next_cursor(
                "thread/list",
                pages,
                &mut seen_cursors,
                response.next_cursor,
            )?
            else {
                break;
            };
            cursor = Some(next);
        }
        Ok(threads)
    }

    pub async fn start_thread(&self, model: &str) -> Result<ThreadSnapshot, SessionError> {
        let params = self.thread_start_params(model);
        let response: ThreadResponse = decode(
            "thread/start",
            self.transport
                .request_default("thread/start", params)
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/start returned an invalid thread snapshot".to_owned())
        })?;
        Ok(response.thread)
    }

    pub async fn resume_thread(
        &self,
        thread_id: &str,
        model: &str,
    ) -> Result<ThreadSnapshot, SessionError> {
        let overrides = self.policy.thread_start_overrides(&self.paths.conversation);
        let response: ThreadResponse = decode(
            "thread/resume",
            self.transport
                .request_default(
                    "thread/resume",
                    ThreadResumeParams {
                        thread_id: thread_id.to_owned(),
                        approval_policy: "never".to_owned(),
                        config: overrides["config"].clone(),
                        cwd: self.paths.conversation.clone(),
                        sandbox: "danger-full-access".to_owned(),
                        model: model.to_owned(),
                    },
                )
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/resume returned an invalid thread snapshot".to_owned())
        })?;
        if response.thread.id != thread_id {
            return Err(SessionError::Protocol(
                "thread/resume returned a different thread id".to_owned(),
            ));
        }
        self.read_thread(thread_id).await
    }

    pub async fn read_thread(&self, thread_id: &str) -> Result<ThreadSnapshot, SessionError> {
        let response: ThreadResponse = decode(
            "thread/read",
            self.transport
                .request_default(
                    "thread/read",
                    ThreadReadParams {
                        thread_id: thread_id.to_owned(),
                        include_turns: true,
                    },
                )
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/read returned an invalid thread snapshot".to_owned())
        })?;
        if response.thread.id != thread_id {
            return Err(SessionError::Protocol(
                "thread/read returned a different thread id".to_owned(),
            ));
        }
        Ok(response.thread)
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<(), SessionError> {
        let _: ThreadDeleteResponse = decode(
            "thread/delete",
            self.transport
                .request_default(
                    "thread/delete",
                    ThreadDeleteParams {
                        thread_id: thread_id.to_owned(),
                    },
                )
                .await?,
        )?;
        Ok(())
    }

    fn thread_start_params(&self, model: &str) -> ThreadStartParams {
        let overrides = self.policy.thread_start_overrides(&self.paths.conversation);
        ThreadStartParams {
            thread_source: ThreadSourceKind::AppServer,
            approval_policy: "never".to_owned(),
            config: overrides["config"].clone(),
            cwd: self.paths.conversation.clone(),
            sandbox: "danger-full-access".to_owned(),
            model: model.to_owned(),
        }
    }
}
