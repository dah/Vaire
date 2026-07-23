use super::*;

impl SessionService {
    pub async fn initialize(&self) -> Result<InitializeResponse, SessionError> {
        let response = self
            .transport
            .request_default("initialize", InitializeParams::vaire())
            .await?;
        let response = decode("initialize", response)?;
        self.transport.notify("initialized", json!({})).await?;
        Ok(response)
    }

    pub async fn next_event(&mut self) -> Option<Result<SessionEvent, SessionError>> {
        let event = self.transport.next_event().await?;
        if event.generation != self.transport.generation() {
            return Some(Ok(SessionEvent::UnknownNotification(
                "stale-generation".to_owned(),
            )));
        }
        use crate::codex::protocol::InboundEvent;
        Some(match event.event {
            InboundEvent::Notification { method, params } => {
                match parse_notification(&method, params) {
                    Ok(Some(event)) => Ok(SessionEvent::Protocol(event)),
                    Ok(None) => Ok(SessionEvent::UnknownNotification(method)),
                    Err(message) => Err(SessionError::Protocol(message)),
                }
            }
            InboundEvent::SafetyViolation { method, .. } => {
                Ok(SessionEvent::SafetyViolation(method))
            }
            InboundEvent::ConnectionClosed { category } => {
                Ok(SessionEvent::ConnectionClosed(category))
            }
        })
    }

    pub async fn shutdown(&mut self) -> Result<(), SessionError> {
        self.transport.shutdown().await?;
        Ok(())
    }
}
