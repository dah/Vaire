use super::*;

pub fn render(frame: &mut Frame<'_>, state: &AppState, ui: &UiState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!(
            "Vairë\nTerminal too small ({}x{}). Resize to at least {MIN_WIDTH}x{MIN_HEIGHT}.\nCtrl-C quits.",
            area.width, area.height
        );
        frame.render_widget(
            Paragraph::new(message)
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let composer_text = sanitize_terminal_text(&ui.composer);
    let content_width = area.width.saturating_sub(2).max(1);
    let composer_wrapped =
        wrap_for_display(&composer_text, content_width).retain_tail_rows(MAX_COMPOSER_ROWS);
    let message = message_text(state);
    let message_wrapped = message
        .as_ref()
        .map(|message| wrap_for_display(&message.text, content_width));
    let (composer_rows, message_rows) = panel_rows(
        area.height,
        composer_wrapped.rows,
        message_wrapped.as_ref().map(|wrapped| wrapped.rows),
    );
    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(composer_rows),
            Constraint::Length(message_rows),
        ])
        .split(area);

    render_header(frame, regions[0], state);
    let activity_frame = state
        .is_waiting_for_assistant_text()
        .then(|| ui.activity_frame());
    if state.thinking.visible {
        let thinking_width = (regions[1].width / 3).clamp(16, 42);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(18), Constraint::Length(thinking_width)])
            .split(regions[1]);
        render_transcript(frame, body[0], state, ui.scroll_from_bottom, activity_frame);
        render_thinking(frame, body[1], state);
    } else {
        render_transcript(
            frame,
            regions[1],
            state,
            ui.scroll_from_bottom,
            activity_frame,
        );
    }
    render_composer(frame, regions[2], &composer_wrapped, state.popup.is_none());
    render_message_or_help(
        frame,
        regions[3],
        message.as_ref(),
        message_wrapped.as_ref(),
    );

    if let Some(overlay) = &ui.overlay {
        render_overlay(frame, area, overlay);
    }
    if let Some(popup) = &state.popup {
        render_popup(frame, area, popup, state, ui.secret_mask());
    }
}

pub(in crate::tui) fn panel_rows(
    terminal_height: u16,
    requested_composer_rows: usize,
    requested_message_rows: Option<usize>,
) -> (u16, u16) {
    let composer_rows = requested_composer_rows.clamp(1, MAX_COMPOSER_ROWS);
    let Some(requested_message_rows) = requested_message_rows else {
        let extra_budget = usize::from(terminal_height.saturating_sub(7));
        let composer_extra = composer_rows.saturating_sub(1).min(extra_budget);
        return ((3 + composer_extra) as u16, 1);
    };

    // Base: status 1 + transcript 2 + composer 3 + message 3 = 9 rows.
    let mut extra_budget = usize::from(terminal_height.saturating_sub(9));
    let requested_message_rows = requested_message_rows.clamp(1, MAX_MESSAGE_ROWS);
    let message_extra = requested_message_rows.saturating_sub(1).min(extra_budget);
    extra_budget = extra_budget.saturating_sub(message_extra);
    let composer_extra = composer_rows.saturating_sub(1).min(extra_budget);
    ((3 + composer_extra) as u16, (3 + message_extra) as u16)
}
