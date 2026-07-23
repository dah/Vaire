use super::*;

pub(in crate::tui) fn render_thread_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    picker: &ThreadPickerState,
    active_provider: crate::provider::ProviderId,
    active_id: Option<&str>,
) {
    let width = area.width.saturating_sub(4).min(92);
    let height = area.height.saturating_sub(2).min(24);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect::new(x, y, width, height);
    frame.render_widget(Clear, popup);

    if let Some(confirmation) = &picker.confirmation {
        let targets = confirmation.targets();
        let mut text = match confirmation {
            ThreadDeleteConfirmation::Selected { target } => format!(
                "Permanently delete this inactive thread?\n\n{}\nID: {}",
                sanitize_terminal_text(&target.title).replace('\n', " "),
                sanitize_terminal_text(&target.id)
            ),
            ThreadDeleteConfirmation::AllInactive { targets } => format!(
                "Permanently delete all {} inactive threads? The active saved thread, if any, is excluded.",
                targets.len()
            ),
        };
        if matches!(confirmation, ThreadDeleteConfirmation::AllInactive { .. }) {
            for target in &targets {
                text.push_str(&format!(
                    "\n• {} [{}]",
                    sanitize_terminal_text(&target.title).replace('\n', " "),
                    sanitize_terminal_text(&target.id)
                ));
            }
        }
        text.push_str("\n\nThis cannot be undone. Press Enter to confirm or Esc to cancel.");
        frame.render_widget(
            Paragraph::new(text)
                .block(
                    Block::default()
                        .title(" Confirm permanent deletion ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red)),
                )
                .style(Style::default().fg(Color::Yellow))
                .wrap(Wrap { trim: false }),
            popup,
        );
        return;
    }

    let block = Block::default()
        .title(" Saved threads & conversations ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let footer_height = inner.height.min(3);
    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
        .split(inner);

    match picker.phase {
        ThreadPickerPhase::Loading => {
            frame.render_widget(
                Paragraph::new("Loading saved Vairë threads…")
                    .style(Style::default().fg(Color::DarkGray)),
                regions[0],
            );
        }
        ThreadPickerPhase::Failed if picker.threads.is_empty() => {
            frame.render_widget(
                Paragraph::new("Threads could not be loaded. Esc closes this window.")
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: false }),
                regions[0],
            );
        }
        _ if picker.threads.is_empty() => {
            frame.render_widget(
                Paragraph::new("No saved threads. Esc closes; /new starts a fresh thread.")
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false }),
                regions[0],
            );
        }
        _ => {
            let items = picker
                .threads
                .iter()
                .map(|thread| {
                    let active =
                        thread.provider == active_provider && active_id == Some(thread.id.as_str());
                    let title = sanitize_terminal_text(&thread.title).replace('\n', " ");
                    let metadata = if active {
                        "ACTIVE — protected".to_owned()
                    } else {
                        format!("inactive — {}", sanitize_terminal_text(&thread.id))
                    };
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            format!("[{}] {title}", thread.provider),
                            Style::default().add_modifier(Modifier::BOLD),
                        )),
                        Line::from(Span::styled(
                            metadata,
                            Style::default().fg(if active {
                                Color::Green
                            } else {
                                Color::DarkGray
                            }),
                        )),
                    ])
                })
                .collect::<Vec<_>>();
            let list = List::new(items).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
            let mut list_state = ListState::default().with_selected(Some(picker.selected));
            frame.render_stateful_widget(list, regions[0], &mut list_state);
        }
    }

    let phase_message = match &picker.phase {
        ThreadPickerPhase::Loading => "Loading…",
        ThreadPickerPhase::Resuming { .. } => "Opening selected thread…",
        ThreadPickerPhase::Deleting { .. } => "Deleting inactive threads…",
        ThreadPickerPhase::Failed => "Load failed",
        ThreadPickerPhase::Ready => {
            "↑/↓ or j/k move • Enter resume • d delete • D clear inactive • Esc close"
        }
    };
    let footer = picker
        .message
        .as_deref()
        .map(|message| format!("{}\n{phase_message}", sanitize_terminal_text(message)))
        .unwrap_or_else(|| phase_message.to_owned());
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: false }),
        regions[1],
    );
}
