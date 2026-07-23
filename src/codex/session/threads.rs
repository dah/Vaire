use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;

#[derive(Default)]
struct ThreadListing {
    seen_threads: HashMap<String, SeenThread>,
    threads: Vec<ThreadListEntry>,
    pages: usize,
    budget: PaginationBudget,
}

struct SeenThread {
    cwd: PathBuf,
    retained: bool,
}

impl ThreadListing {
    fn retain(&mut self, thread: ThreadListEntry, expected_cwd: &Path) -> Result<(), SessionError> {
        if !valid_identifier(&thread.id) {
            return Err(SessionError::Protocol(
                "thread/list returned an invalid thread id".to_owned(),
            ));
        }
        if thread.cwd != expected_cwd {
            return Err(SessionError::Protocol(
                "thread/list returned a thread outside the requested Vairë working directory"
                    .to_owned(),
            ));
        }
        if !thread.source.is_supported_resume_source() {
            return Err(SessionError::Protocol(
                "thread/list returned a thread from an unsupported source".to_owned(),
            ));
        }
        let already_retained = match self.seen_threads.get(&thread.id) {
            Some(seen) if seen.cwd != expected_cwd => {
                return Err(SessionError::Protocol(
                    "thread/list returned the same thread id under conflicting working directories"
                        .to_owned(),
                ));
            }
            Some(seen) => seen.retained,
            None => {
                self.budget
                    .retain("thread/list", thread_origin_retained_bytes(&thread))?;
                self.seen_threads.insert(
                    thread.id.clone(),
                    SeenThread {
                        cwd: expected_cwd.to_path_buf(),
                        retained: false,
                    },
                );
                false
            }
        };

        if thread.ephemeral || already_retained {
            return Ok(());
        }

        self.budget
            .retain_additional_bytes("thread/list", thread_additional_retained_bytes(&thread))?;
        self.seen_threads
            .get_mut(&thread.id)
            .expect("validated thread origin must be retained")
            .retained = true;
        self.threads.push(thread);
        Ok(())
    }
}

impl SessionService {
    pub async fn list_threads(&self) -> Result<Vec<ThreadListEntry>, SessionError> {
        let mut listing = ThreadListing::default();
        self.list_threads_for_cwd(&self.paths.conversation, &mut listing)
            .await?;

        if let Some(historical_cwd) = self
            .paths
            .historical_conversation
            .as_deref()
            .filter(|cwd| *cwd != self.paths.conversation)
        {
            self.list_threads_for_cwd(historical_cwd, &mut listing)
                .await?;
        }

        Ok(listing.threads)
    }

    async fn list_threads_for_cwd(
        &self,
        cwd: &Path,
        listing: &mut ThreadListing,
    ) -> Result<(), SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();

        loop {
            if listing.pages >= MAX_PAGINATION_PAGES {
                return Err(SessionError::Protocol(
                    "thread/list exceeded the pagination limit".to_owned(),
                ));
            }

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
                            cwd: cwd.to_path_buf(),
                            limit: 50,
                            sort_direction: "desc".to_owned(),
                            sort_key: "updated_at".to_owned(),
                        },
                    )
                    .await?,
            )?;
            listing.pages += 1;
            validate_page_len("thread/list", response.data.len(), MAX_THREAD_PAGE_ITEMS)?;
            for thread in response.data {
                listing.retain(thread, cwd)?;
            }
            let Some(next) = next_cursor(
                "thread/list",
                listing.pages,
                &mut seen_cursors,
                response.next_cursor,
            )?
            else {
                break;
            };
            cursor = Some(next);
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::protocol::{SessionSource, SessionSourceName};

    fn listed_thread(id: &str, cwd: &Path) -> ThreadListEntry {
        ThreadListEntry {
            id: id.to_owned(),
            name: None,
            preview: String::new(),
            created_at: 1,
            updated_at: 1,
            cwd: cwd.to_path_buf(),
            ephemeral: false,
            source: SessionSource::Named(SessionSourceName::AppServer),
        }
    }

    #[test]
    fn current_and_historical_results_share_the_retained_text_budget() {
        let current = Path::new("/tmp/current");
        let historical = Path::new("/tmp/historical");
        let current_thread = listed_thread("thr-current", current);
        let current_bytes = thread_retained_bytes(&current_thread);
        let mut listing = ThreadListing::default();

        listing.retain(current_thread, current).unwrap();
        listing
            .budget
            .retain("thread/list", MAX_PAGINATION_RETAINED_BYTES - current_bytes)
            .unwrap();

        assert!(matches!(
            listing.retain(listed_thread("thr-historical", historical), historical),
            Err(SessionError::Protocol(message)) if message.contains("retained byte limit")
        ));
    }

    #[test]
    fn current_and_historical_results_share_the_retained_item_budget() {
        let current = Path::new("/tmp/current");
        let historical = Path::new("/tmp/historical");
        let mut listing = ThreadListing::default();

        listing
            .retain(listed_thread("thr-current", current), current)
            .unwrap();
        for _ in 1..MAX_PAGINATION_ITEMS {
            listing.budget.retain("thread/list", 0).unwrap();
        }

        assert!(matches!(
            listing.retain(listed_thread("thr-historical", historical), historical),
            Err(SessionError::Protocol(message)) if message.contains("retained item limit")
        ));
    }

    #[test]
    fn ephemeral_occurrences_do_not_hide_cross_cwd_id_conflicts() {
        let current = Path::new("/tmp/current");
        let historical = Path::new("/tmp/historical");

        for ephemeral_first in [true, false] {
            let mut listing = ThreadListing::default();
            let mut first = listed_thread("thr-conflict", current);
            first.ephemeral = ephemeral_first;
            listing.retain(first, current).unwrap();

            let mut second = listed_thread("thr-conflict", historical);
            second.ephemeral = !ephemeral_first;
            assert!(matches!(
                listing.retain(second, historical),
                Err(SessionError::Protocol(message))
                    if message.contains("conflicting working directories")
            ));
        }
    }

    #[test]
    fn same_cwd_non_ephemeral_occurrence_promotes_an_ephemeral_origin() {
        let cwd = Path::new("/tmp/current");
        let mut listing = ThreadListing::default();
        let mut ephemeral = listed_thread("thr-promoted", cwd);
        ephemeral.ephemeral = true;
        listing.retain(ephemeral, cwd).unwrap();

        let mut retained = listed_thread("thr-promoted", cwd);
        retained.name = Some("Retained".to_owned());
        retained.preview = "visible history".to_owned();
        listing.retain(retained, cwd).unwrap();

        assert_eq!(listing.threads.len(), 1);
        assert_eq!(listing.threads[0].preview, "visible history");
    }

    #[test]
    fn ephemeral_origin_and_promotion_share_one_item_and_charge_all_bytes() {
        let cwd = Path::new("/tmp/current");
        let mut listing = ThreadListing::default();
        let mut ephemeral = listed_thread("thr-budget", cwd);
        ephemeral.ephemeral = true;
        let origin_bytes = thread_origin_retained_bytes(&ephemeral);
        listing.retain(ephemeral, cwd).unwrap();

        for _ in 1..MAX_PAGINATION_ITEMS {
            listing.budget.retain("thread/list", 0).unwrap();
        }

        let mut retained = listed_thread("thr-budget", cwd);
        retained.name = Some("Retained".to_owned());
        retained.preview = "visible history".to_owned();
        let additional_bytes = thread_additional_retained_bytes(&retained);
        listing
            .budget
            .retain_additional_bytes(
                "thread/list",
                MAX_PAGINATION_RETAINED_BYTES - origin_bytes - additional_bytes,
            )
            .unwrap();

        listing.retain(retained, cwd).unwrap();
        assert_eq!(listing.threads.len(), 1);
        assert!(matches!(
            listing
                .budget
                .retain_additional_bytes("thread/list", 1),
            Err(SessionError::Protocol(message)) if message.contains("retained byte limit")
        ));
    }
}
