use super::*;

pub fn parse_notification(method: &str, params: Value) -> Result<Option<ProtocolEvent>, String> {
    fn decode<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, String> {
        serde_json::from_value(params)
            .map_err(|_| format!("{method} notification had invalid required fields"))
    }

    let invalid = || format!("{method} notification had invalid required fields");
    let event = match method {
        "account/login/completed" => ProtocolEvent::AccountLoginCompleted(decode(method, params)?),
        "account/updated" => ProtocolEvent::AccountUpdated,
        "thread/started" => {
            #[derive(Deserialize)]
            struct Body {
                thread: ThreadSnapshot,
            }
            let thread = decode::<Body>(method, params)?.thread;
            validate_thread_snapshot(&thread).map_err(|_| invalid())?;
            ProtocolEvent::ThreadStarted(thread)
        }
        "turn/started" => {
            let notification: TurnNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id]).map_err(|_| invalid())?;
            validate_turn_snapshot(&notification.turn).map_err(|_| invalid())?;
            if notification.turn.status != TurnStatus::InProgress {
                return Err(invalid());
            }
            ProtocolEvent::TurnStarted(notification)
        }
        "item/agentMessage/delta" => {
            let notification: AgentMessageDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::AgentMessageDelta(notification)
        }
        "item/reasoning/summaryTextDelta" => {
            let notification: ReasoningSummaryTextDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningSummaryTextDelta(notification)
        }
        "item/reasoning/summaryPartAdded" => {
            let notification: ReasoningSummaryPartAddedNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningSummaryPartAdded(notification)
        }
        "item/reasoning/textDelta" => {
            let notification: ReasoningTextDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningTextDelta(notification)
        }
        "item/completed" => {
            let notification: ItemCompletedNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            validate_thread_item(&notification.item).map_err(|_| invalid())?;
            ProtocolEvent::ItemCompleted(notification)
        }
        "turn/completed" => {
            let notification: TurnNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id]).map_err(|_| invalid())?;
            validate_turn_snapshot(&notification.turn).map_err(|_| invalid())?;
            ProtocolEvent::TurnCompleted(notification)
        }
        "thread/tokenUsage/updated" => {
            let notification: ThreadTokenUsageUpdatedNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            ProtocolEvent::ThreadTokenUsageUpdated(notification)
        }
        "error" => {
            let notification: ErrorNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            ProtocolEvent::Error(notification)
        }
        _ => return Ok(None),
    };
    Ok(Some(event))
}
