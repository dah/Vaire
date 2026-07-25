use super::*;

mod auth;
mod claude;
mod codex_turn;
mod conversations;
mod openrouter;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn execute_effects(
        &mut self,
        initial: Vec<Effect>,
    ) -> Result<(), BackendError> {
        let mut effects = VecDeque::from(initial);
        while let Some(effect) = effects.pop_front() {
            let produced = match effect {
                Effect::StartLogin => self.start_login_effect().await?,
                Effect::StartDeviceLogin => self.start_device_login_effect().await?,
                Effect::CancelLogin { login_id } => self.cancel_login_effect(login_id).await?,
                Effect::Logout => self.logout_effect().await?,
                Effect::StartNewThread => self.start_new_thread_effect().await?,
                Effect::StartNewOpenRouterConversation => {
                    self.start_new_openrouter_conversation_effect().await
                }
                Effect::ListThreads => self.list_threads_effect().await,
                Effect::ResumeThread { id } => self.resume_thread_effect(id).await?,
                Effect::SwitchThread {
                    id,
                    model,
                    reasoning,
                } => self.switch_thread_effect(id, model, reasoning).await?,
                Effect::SwitchOpenRouterConversation { id, model } => {
                    self.switch_openrouter_conversation_effect(id, model).await
                }
                Effect::DeleteThreads { ids } => self.delete_threads(ids).await,
                Effect::DeleteOpenRouterConversations { ids } => {
                    self.delete_conversations(Vec::new(), ids).await
                }
                Effect::DeleteConversations {
                    codex_ids,
                    openrouter_ids,
                } => self.delete_conversations(codex_ids, openrouter_ids).await,
                Effect::SendMessage { text } => match self.send_message(&text).await {
                    Ok(effects) => effects,
                    Err(error) => self.reduce_mutating_error(error),
                },
                Effect::SendOpenRouterMessage { text } => {
                    self.send_openrouter_message_effect(text).await
                }
                Effect::RefreshOpenRouter => self.refresh_openrouter_effect(),
                Effect::LogoutOpenRouter => self.logout_openrouter_effect().await,
                Effect::InterruptOpenRouterTurn => self.interrupt_openrouter_turn_effect(),
                Effect::LoginClaude => self.login_claude_effect(),
                Effect::RefreshClaude => self.refresh_claude_effect().await,
                Effect::LogoutClaude => self.logout_claude_effect().await,
                Effect::StartNewClaudeSession => self.start_new_claude_session_effect().await,
                Effect::SwitchClaudeSession { id } => {
                    // Resume restores the registered session's own alias. Alias changes create a
                    // new blank session at the reducer boundary.
                    self.switch_claude_session_effect(id).await
                }
                Effect::SendClaudeMessage { text, effort } => {
                    self.send_claude_message_effect(text, effort).await
                }
                Effect::InterruptClaudeTurn => self.interrupt_claude_turn_effect(),
                Effect::DeleteClaudeSessions { ids } => {
                    self.delete_all_conversations(Vec::new(), Vec::new(), ids)
                        .await
                }
                Effect::DeleteAllConversations {
                    codex_ids,
                    openrouter_ids,
                    claude_ids,
                } => {
                    self.delete_all_conversations(codex_ids, openrouter_ids, claude_ids)
                        .await
                }
                Effect::InterruptTurn { thread_id, turn_id } => {
                    self.interrupt_codex_turn_effect(thread_id, turn_id).await?
                }
                Effect::Persist(preferences) => {
                    let _ = self.persist_preferences(&preferences)?;
                    Vec::new()
                }
                Effect::Shutdown => {
                    self.shutdown().await?;
                    Vec::new()
                }
            };
            effects.extend(produced);
        }
        Ok(())
    }
}
