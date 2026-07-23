use super::*;

pub(in crate::tui) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    scroll_from_bottom: usize,
    activity_frame: Option<&'static str>,
) {
    let mut lines = Vec::<Line<'static>>::new();
    if state.transcript.is_empty() && activity_frame.is_none() {
        let prompt = if state.active_provider == crate::provider::ProviderId::OpenRouter {
            match (&state.openrouter.auth, &state.openrouter.conversation) {
                (crate::openrouter::OpenRouterAuthStatus::Missing, _) => {
                    "OpenRouter is signed out. Use /login to enter an API key."
                }
                (_, crate::app::OpenRouterConversationState::ResumeFailed { .. }) => {
                    "The saved OpenRouter conversation could not be resumed. Use /resume or /new; it was not replaced."
                }
                _ => "OpenRouter ready. Type a message, use /new, or browse history with /resume.",
            }
        } else {
            match (&state.auth, &state.thread) {
            (AuthState::SignedOut, _) => {
                "Signed out. Use /login to connect your ChatGPT subscription."
            }
            (_, ThreadState::ResumeFailed { .. }) => {
                "The saved thread could not be resumed. Use /resume to choose a thread or /new to start fresh; it was not replaced."
            }
            _ => "Ready. Type a message, use /new, or browse saved threads with /resume.",
            }
        };
        lines.push(Line::from(Span::styled(
            prompt,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for entry in &state.transcript {
            let (prefix, color) = match entry.role {
                TranscriptRole::User => ("You", Color::Green),
                TranscriptRole::Assistant => ("Agent", Color::Cyan),
            };
            lines.push(Line::from(Span::styled(
                format!("{prefix}:"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));
            let text = sanitize_terminal_text(&entry.text);
            if text.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.extend(text.lines().map(|line| Line::from(line.to_owned())));
            }
            lines.push(Line::from(""));
        }
    }
    if let Some(activity_frame) = activity_frame {
        lines.push(Line::from(vec![
            Span::styled(
                "Agent:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(activity_frame, Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(""));
    }

    let block = Block::default()
        .title(" Conversation ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    let window = paragraph_window(lines, inner.width, inner.height, scroll_from_bottom);
    frame.render_widget(
        Paragraph::new(window.lines)
            .wrap(Wrap { trim: false })
            .block(block)
            .scroll((window.scroll, 0)),
        area,
    );
}

pub(in crate::tui) fn render_thinking(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if state.active_provider == crate::provider::ProviderId::OpenRouter {
        frame.render_widget(
            Paragraph::new("OpenRouter reasoning is not collected in this milestone.")
                .block(Block::default().title(" Reasoning ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let mut lines = vec![Line::from(Span::styled(
        "Only reasoning content explicitly emitted by Codex is shown.",
        Style::default().fg(Color::DarkGray),
    ))];
    let populated = state
        .thinking
        .entries
        .iter()
        .filter(|entry| !entry.text.is_empty())
        .collect::<Vec<_>>();

    if populated.is_empty() {
        let message = match &state.turn {
            TurnState::Starting | TurnState::Streaming { .. } => "Awaiting emitted reasoning…",
            TurnState::Failed { .. } => "No reasoning content was emitted before this turn failed.",
            _ => "No reasoning content was emitted for this turn.",
        };
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            message,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for entry in populated {
            lines.push(Line::from(""));
            let label = match entry.kind {
                ThinkingKind::Summary => "Summary",
                ThinkingKind::EmittedText => "Reasoning text",
            };
            lines.push(Line::from(Span::styled(
                format!("{label}:"),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
            let text = sanitize_terminal_text(&entry.text);
            lines.extend(text.lines().map(|line| Line::from(line.to_owned())));
        }
        if matches!(state.turn, TurnState::Failed { .. }) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Turn failed.",
                Style::default().fg(Color::Red),
            )));
        }
    }

    let block = Block::default().title(" Reasoning ").borders(Borders::ALL);
    let inner = block.inner(area);
    let window = paragraph_window(lines, inner.width, inner.height, 0);
    frame.render_widget(
        Paragraph::new(window.lines)
            .wrap(Wrap { trim: false })
            .block(block)
            .scroll((window.scroll, 0)),
        area,
    );
}
