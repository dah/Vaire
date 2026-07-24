use super::*;

mod auth;
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
