use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::tui) struct MessageText {
    pub(in crate::tui) title: &'static str,
    pub(in crate::tui) text: String,
    pub(in crate::tui) failure: bool,
}

pub(in crate::tui) fn message_text(state: &AppState) -> Option<MessageText> {
    if let TurnState::Failed { message, .. } = &state.turn {
        let failure = sanitize_terminal_text(message);
        let notice = state.notice.as_deref().map(sanitize_terminal_text);
        let text = notice
            .filter(|notice| notice != &failure)
            .map_or_else(|| failure.clone(), |notice| format!("{failure}\n{notice}"));
        return Some(MessageText {
            title: " Turn failed ",
            text,
            failure: true,
        });
    }
    state.notice.as_deref().map(|notice| MessageText {
        title: " Message ",
        text: sanitize_terminal_text(notice),
        failure: false,
    })
}

pub(in crate::tui) fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(
        Paragraph::new(header_text(state, area.width)).style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

pub(in crate::tui) fn header_text(state: &AppState, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let context = state.context_remaining_percent.map_or_else(
        || "Context --".to_owned(),
        |percent| format!("Context {percent}%"),
    );
    let context_width = UnicodeWidthStr::width(context.as_str());
    if context_width >= width {
        return truncate_for_display(&context, width);
    }

    let left_capacity = width.saturating_sub(context_width + 1);
    let left = truncate_for_display(&status_text(state), left_capacity);
    let padding = width
        .saturating_sub(UnicodeWidthStr::width(left.as_str()))
        .saturating_sub(context_width);
    format!("{left}{}{context}", " ".repeat(padding))
}

pub(in crate::tui) fn status_text(state: &AppState) -> String {
    let connection = match &state.connection {
        ConnectionState::Disconnected => "offline".to_owned(),
        ConnectionState::Connecting => "connecting".to_owned(),
        ConnectionState::Ready { .. } => "connected".to_owned(),
        ConnectionState::Failed(message) => format!("error: {message}"),
    };
    let auth = match &state.auth {
        AuthState::Unknown => "auth?".to_owned(),
        AuthState::SignedOut => "signed out".to_owned(),
        AuthState::SigningIn { .. } => "signing in".to_owned(),
        AuthState::SignedIn {
            scope: Some(crate::persistence::AccountScope::ChatgptEmail(email)),
        } => email.clone(),
        AuthState::SignedIn { scope: None } => "account?".to_owned(),
        AuthState::Unsupported(message) => format!("unsupported: {message}"),
    };
    let thread = match &state.thread {
        ThreadState::None => "no thread".to_owned(),
        ThreadState::Resuming { .. } => "resuming".to_owned(),
        ThreadState::Ready { .. } => "thread ready".to_owned(),
        ThreadState::ResumeFailed { .. } => "resume failed".to_owned(),
        ThreadState::AccountMismatch { .. } => "account mismatch".to_owned(),
    };
    let turn = match &state.turn {
        TurnState::Idle => "idle",
        TurnState::Starting => "starting",
        TurnState::Streaming { .. } => "streaming",
        TurnState::Completed { .. } => "completed",
        TurnState::Interrupted { .. } => "interrupted",
        TurnState::Failed { .. } => "failed",
    };
    let model = state
        .selected_model
        .clone()
        .unwrap_or_else(|| "model?".to_owned());
    let reasoning = state
        .selected_reasoning
        .clone()
        .unwrap_or_else(|| "reasoning?".to_owned());
    let shutdown = if state.shutting_down {
        " • shutting down"
    } else {
        ""
    };
    sanitize_header_text(&format!(
        " {connection} • {auth} • {thread} • {model}/{reasoning} • {turn}{shutdown}"
    ))
}

pub(in crate::tui) fn sanitize_header_text(value: &str) -> String {
    sanitize_terminal_text(value).replace('\n', " ")
}

pub(in crate::tui) fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    wrapped: &WrappedText,
    show_cursor: bool,
) {
    let block = Block::default().title(" Message ").borders(Borders::ALL);
    let inner = block.inner(area);
    let scroll = wrapped.rows.saturating_sub(inner.height as usize);
    frame.render_widget(
        Paragraph::new(wrapped.text.clone())
            .block(block)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
    let last_line_width = wrapped.tail_column.min(u16::MAX as usize) as u16;
    let visible_tail_row = wrapped
        .rows
        .saturating_sub(1)
        .saturating_sub(scroll)
        .min(u16::MAX as usize) as u16;
    let x = inner
        .x
        .saturating_add(last_line_width.min(inner.width.saturating_sub(1)));
    let y = inner
        .y
        .saturating_add(visible_tail_row.min(inner.height.saturating_sub(1)));
    if show_cursor {
        frame.set_cursor_position((x, y));
    }
}

pub(in crate::tui) fn render_message_or_help(
    frame: &mut Frame<'_>,
    area: Rect,
    message: Option<&MessageText>,
    wrapped: Option<&WrappedText>,
) {
    let (Some(message), Some(wrapped)) = (message, wrapped) else {
        frame.render_widget(
            Paragraph::new(
                "Enter send • Alt-Enter newline • PgUp/PgDn scroll • Esc interrupt/help • /help",
            )
            .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    };
    let color = if message.failure {
        Color::Red
    } else {
        Color::Yellow
    };
    frame.render_widget(
        Paragraph::new(wrapped.text.clone())
            .block(Block::default().title(message.title).borders(Borders::ALL))
            .style(Style::default().fg(color)),
        area,
    );
}

pub(in crate::tui) fn render_overlay(frame: &mut Frame<'_>, area: Rect, value: &str) {
    let width = area.width.saturating_sub(6).min(76);
    let height = area.height.saturating_sub(4).min(16);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(sanitize_terminal_text(value))
            .block(
                Block::default()
                    .title(" Help / message — Esc closes ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}
