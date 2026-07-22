use super::*;

impl SessionService {
    pub async fn start_turn(
        &self,
        thread_id: &str,
        text: &str,
        model: &str,
        effort: &str,
    ) -> Result<TurnStartResponse, SessionError> {
        let overrides = self.policy.turn_start_overrides(&self.paths.conversation);
        let response: TurnStartResponse = decode(
            "turn/start",
            self.transport
                .request_default(
                    "turn/start",
                    TurnStartParams {
                        thread_id: thread_id.to_owned(),
                        input: vec![UserInput::text(text)],
                        model: model.to_owned(),
                        effort: effort.to_owned(),
                        summary: ReasoningSummary::Detailed,
                        approval_policy: "never".to_owned(),
                        cwd: self.paths.conversation.clone(),
                        sandbox_policy: overrides["sandboxPolicy"].clone(),
                    },
                )
                .await?,
        )?;
        validate_turn_snapshot(&response.turn).map_err(|_| {
            SessionError::Protocol("turn/start returned an invalid turn snapshot".to_owned())
        })?;
        if response.turn.status != crate::codex::protocol::TurnStatus::InProgress {
            return Err(SessionError::Protocol(
                "turn/start returned a terminal or unknown turn status".to_owned(),
            ));
        }
        Ok(response)
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), SessionError> {
        let _: TurnInterruptResponse = decode(
            "turn/interrupt",
            self.transport
                .request_default(
                    "turn/interrupt",
                    TurnInterruptParams {
                        thread_id: thread_id.to_owned(),
                        turn_id: turn_id.to_owned(),
                    },
                )
                .await?,
        )?;
        Ok(())
    }
}
