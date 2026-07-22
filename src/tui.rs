use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{
        AppState, AuthState, ConnectionState, Intent, ThinkingKind, ThreadDeleteConfirmation,
        ThreadPickerPhase, ThreadPickerState, ThreadState, TranscriptRole, TurnState,
    },
    command::{parse, HELP_TEXT},
};

pub use crate::text::sanitize_terminal_text;

const MIN_WIDTH: u16 = 36;
const MIN_HEIGHT: u16 = 9;
const MAX_COMPOSER_ROWS: usize = 5;
const MAX_MESSAGE_ROWS: usize = 4;
const ACTIVITY_TICKS_PER_FRAME: u8 = 4;
const ACTIVITY_FRAMES: [&str; 10] = [
    "~    ", "~~   ", "~~~  ", " ~~~ ", "  ~~~", "   ~~", "    ~", "   ~~", "  ~~~", " ~~~ ",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ActivityAnimation {
    active: bool,
    frame_index: usize,
    ticks_in_frame: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiState {
    pub composer: String,
    pub overlay: Option<String>,
    /// Number of wrapped transcript rows held above the automatic bottom position.
    pub scroll_from_bottom: usize,
    activity: ActivityAnimation,
}

impl UiState {
    /// Synchronizes ephemeral animation state after an application-state update.
    ///
    /// The return value tells an event loop whether the visible animation changed.
    pub fn sync_activity_animation(&mut self, state: &AppState) -> bool {
        let needed = state.is_waiting_for_assistant_text();
        match (self.activity.active, needed) {
            (false, true) => {
                self.activity.active = true;
                self.activity.frame_index = 0;
                self.activity.ticks_in_frame = 0;
                true
            }
            (true, false) => {
                self.activity = ActivityAnimation::default();
                true
            }
            _ => false,
        }
    }

    /// Advances the animation from the event loop's existing 33 ms tick.
    ///
    /// It returns `true` only when a redraw is necessary: animation activation, a new frame, or
    /// removal after the surrounding state stopped needing the indicator.
    pub fn advance_activity_animation(&mut self, state: &AppState) -> bool {
        if !state.is_waiting_for_assistant_text() {
            return if self.activity.active {
                self.activity = ActivityAnimation::default();
                true
            } else {
                false
            };
        }
        if !self.activity.active {
            self.activity.active = true;
            self.activity.frame_index = 0;
            self.activity.ticks_in_frame = 0;
            return true;
        }

        self.activity.ticks_in_frame = self.activity.ticks_in_frame.saturating_add(1);
        if self.activity.ticks_in_frame < ACTIVITY_TICKS_PER_FRAME {
            return false;
        }
        self.activity.ticks_in_frame = 0;
        self.activity.frame_index = (self.activity.frame_index + 1) % ACTIVITY_FRAMES.len();
        true
    }

    fn activity_frame(&self) -> &'static str {
        ACTIVITY_FRAMES[self.activity.frame_index]
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Intent> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.composer.push_str(&sanitize_terminal_text(&text));
                None
            }
            _ => None,
        }
    }

    pub fn handle_event_for_state(&mut self, event: Event, state: &AppState) -> Option<Intent> {
        if state.thread_picker.is_none() {
            return self.handle_event(event);
        }
        match event {
            Event::Key(key) => self.handle_thread_picker_key(key, state),
            _ => None,
        }
    }

    fn handle_thread_picker_key(&mut self, key: KeyEvent, state: &AppState) -> Option<Intent> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Intent::Quit);
        }
        if self.overlay.is_some() {
            if key.code == KeyCode::Esc {
                self.overlay = None;
            }
            return None;
        }
        let picker = state.thread_picker.as_ref()?;
        if picker.confirmation.is_some() {
            return match key.code {
                KeyCode::Enter => Some(Intent::ThreadPickerConfirmDelete),
                KeyCode::Esc => Some(Intent::ThreadPickerCancelDelete),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Esc => Some(Intent::ThreadPickerClose),
            KeyCode::Up | KeyCode::Char('k') => Some(Intent::ThreadPickerMoveUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Intent::ThreadPickerMoveDown),
            KeyCode::Enter => Some(Intent::ThreadPickerSelect),
            KeyCode::Char('D') => Some(Intent::ThreadPickerRequestClearInactive),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(Intent::ThreadPickerRequestClearInactive)
            }
            KeyCode::Char('d') => Some(Intent::ThreadPickerRequestDelete),
            _ => None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Intent> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Intent::Quit);
        }
        match key.code {
            KeyCode::Esc => {
                if self.overlay.take().is_some() {
                    None
                } else {
                    Some(Intent::Interrupt)
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.push('\n');
                None
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.composer.pop();
                None
            }
            KeyCode::PageUp | KeyCode::Up => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
                None
            }
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
                None
            }
            KeyCode::Home => {
                self.scroll_from_bottom = usize::MAX / 2;
                None
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                None
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() =>
            {
                self.composer.push(character);
                None
            }
            _ => None,
        }
    }

    fn submit(&mut self) -> Option<Intent> {
        match parse(&self.composer) {
            Ok(Intent::Help) => {
                self.overlay = Some(HELP_TEXT.to_owned());
                self.composer.clear();
                None
            }
            Ok(intent) => {
                self.composer.clear();
                self.scroll_from_bottom = 0;
                Some(intent)
            }
            Err(error) => {
                self.overlay = Some(error.to_string());
                None
            }
        }
    }
}

pub fn render(frame: &mut Frame<'_>, state: &AppState, ui: &UiState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!(
            "AgentHarness\nTerminal too small ({}x{}). Resize to at least {MIN_WIDTH}x{MIN_HEIGHT}.\nCtrl-C quits.",
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
    let composer_wrapped = wrap_for_display(&composer_text, content_width);
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
    render_composer(
        frame,
        regions[2],
        &composer_wrapped,
        state.thread_picker.is_none(),
    );
    render_message_or_help(
        frame,
        regions[3],
        message.as_ref(),
        message_wrapped.as_ref(),
    );

    if let Some(overlay) = &ui.overlay {
        render_overlay(frame, area, overlay);
    }
    if let Some(picker) = &state.thread_picker {
        render_thread_picker(frame, area, picker, state.preferences.thread_id.as_deref());
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WrappedText {
    text: String,
    rows: usize,
    tail_column: usize,
}

fn wrap_for_display(value: &str, width: u16) -> WrappedText {
    let width = usize::from(width.max(1));
    let mut text = String::with_capacity(value.len());
    let mut row = 0_usize;
    let mut column = 0_usize;
    for character in value.chars() {
        if character == '\n' {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if column > 0 && column.saturating_add(character_width) > width {
            text.push('\n');
            row = row.saturating_add(1);
            column = 0;
        }
        text.push(character);
        column = column.saturating_add(character_width);
    }
    WrappedText {
        text,
        rows: row.saturating_add(1),
        tail_column: column,
    }
}

fn panel_rows(
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct MessageText {
    title: &'static str,
    text: String,
    failure: bool,
}

fn message_text(state: &AppState) -> Option<MessageText> {
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

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
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

fn header_text(state: &AppState, width: u16) -> String {
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

fn status_text(state: &AppState) -> String {
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

fn sanitize_header_text(value: &str) -> String {
    sanitize_terminal_text(value).replace('\n', " ")
}

fn truncate_for_display(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let content_width = width.saturating_sub(1);
    let mut truncated = String::with_capacity(value.len().min(width));
    let mut used = 0_usize;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used.saturating_add(character_width) > content_width {
            break;
        }
        truncated.push(character);
        used = used.saturating_add(character_width);
    }
    truncated.push('…');
    truncated
}

fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    scroll_from_bottom: usize,
    activity_frame: Option<&'static str>,
) {
    let mut lines = Vec::<Line<'static>>::new();
    if state.transcript.is_empty() && activity_frame.is_none() {
        let prompt = match (&state.auth, &state.thread) {
            (AuthState::SignedOut, _) => {
                "Signed out. Use /login to connect your ChatGPT subscription."
            }
            (_, ThreadState::ResumeFailed { .. }) => {
                "The saved thread could not be resumed. Use /resume to choose a thread or /new to start fresh; it was not replaced."
            }
            _ => "Ready. Type a message, use /new, or browse saved threads with /resume.",
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
    let wrap_width = inner.width.max(1) as usize;
    let line_count = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(wrap_width))
        .sum::<usize>();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    let max_top = line_count.saturating_sub(inner.height as usize);
    let from_bottom = scroll_from_bottom.min(max_top);
    let top = max_top.saturating_sub(from_bottom).min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.scroll((top, 0)), area);
}

fn render_thinking(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
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
    let wrap_width = inner.width.max(1) as usize;
    let line_count = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(wrap_width))
        .sum::<usize>();
    let top = line_count
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((top, 0)),
        area,
    );
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, wrapped: &WrappedText, show_cursor: bool) {
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

fn render_message_or_help(
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

fn render_overlay(frame: &mut Frame<'_>, area: Rect, value: &str) {
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

fn render_thread_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    picker: &ThreadPickerState,
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
        .title(" Saved threads ")
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
                Paragraph::new("Loading saved AgentHarness threads…")
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
                    let active = active_id == Some(thread.id.as_str());
                    let title = sanitize_terminal_text(&thread.title).replace('\n', " ");
                    let metadata = if active {
                        "ACTIVE — protected".to_owned()
                    } else {
                        format!("inactive — {}", sanitize_terminal_text(&thread.id))
                    };
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            title,
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

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::Backend, backend::TestBackend, layout::Position, Terminal};
    use unicode_width::UnicodeWidthStr;

    use super::{
        header_text, render, sanitize_terminal_text, UiState, ACTIVITY_FRAMES,
        ACTIVITY_TICKS_PER_FRAME,
    };
    use crate::app::{
        Action, AppState, AuthState, ConnectionState, DomainEvent, Intent, ThinkingEntry,
        ThinkingKind, ThreadChoice, ThreadDeleteConfirmation, ThreadPickerPhase, ThreadPickerState,
        ThreadState, TranscriptEntry, TranscriptRole, TurnState,
    };
    use crate::persistence::AccountScope;

    fn draw(state: &AppState, ui: &UiState, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state, ui)).unwrap();
        terminal
    }

    fn screen(state: &AppState, ui: &UiState, width: u16, height: u16) -> String {
        let terminal = draw(state, ui, width, height);
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn header(state: &AppState, width: u16) -> String {
        screen(state, &UiState::default(), width, 20)
            .lines()
            .next()
            .unwrap()
            .to_owned()
    }

    fn ready() -> AppState {
        AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("user@example.com"),
            },
            thread: ThreadState::Ready {
                id: "thread".to_owned(),
            },
            selected_model: Some("model-a".to_owned()),
            selected_reasoning: Some("high".to_owned()),
            ..AppState::default()
        }
    }

    fn waiting() -> AppState {
        let mut state = ready();
        state.turn = TurnState::Starting;
        state.transcript.push(TranscriptEntry {
            role: TranscriptRole::User,
            text: "hello".to_owned(),
            item_id: None,
            turn_id: None,
        });
        state
    }

    #[test]
    fn renders_signed_out_and_resume_failed_actions() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedOut,
            ..AppState::default()
        };
        assert!(screen(&state, &UiState::default(), 90, 20).contains("/login"));
        state.auth = AuthState::SignedIn { scope: None };
        state.thread = ThreadState::ResumeFailed {
            id: "secret-id".to_owned(),
            message: "gone".to_owned(),
        };
        let rendered = screen(&state, &UiState::default(), 90, 20);
        assert!(rendered.contains("resume failed"));
        assert!(rendered.contains("/resume"));
        assert!(!rendered.contains("secret-id"));
    }

    #[test]
    fn login_completion_is_visible_without_follow_up_input() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            notice: Some("Complete sign-in in the browser".to_owned()),
            ..AppState::default()
        };

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("User@Example.COM"),
        )));

        let rendered = screen(&state, &UiState::default(), 90, 20);
        assert!(header(&state, 90).contains("user@example.com"));
        assert!(rendered.contains("Signed in to ChatGPT"));
        assert!(!rendered.contains("Complete sign-in"));
    }

    #[test]
    fn header_preserves_auth_labels_and_replaces_identity_on_account_changes() {
        let mut state = ready();
        state.auth = AuthState::Unknown;
        assert!(header(&state, 100).contains("auth?"));

        state.auth = AuthState::SignedOut;
        assert!(header(&state, 100).contains("signed out"));

        state.auth = AuthState::SigningIn {
            login_id: "login-active".to_owned(),
        };
        assert!(header(&state, 100).contains("signing in"));

        state.auth = AuthState::Unsupported("apiKey".to_owned());
        assert!(header(&state, 100).contains("unsupported: apiKey"));

        state.auth = AuthState::SignedIn { scope: None };
        assert!(header(&state, 100).contains("account?"));

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("first@example.com"),
        )));
        assert!(header(&state, 100).contains("first@example.com"));

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("second@example.com"),
        )));
        let switched = header(&state, 100);
        assert!(switched.contains("second@example.com"));
        assert!(!switched.contains("first@example.com"));

        state.reduce(Action::Event(DomainEvent::LoggedOut));
        let logged_out = header(&state, 100);
        assert!(logged_out.contains("signed out"));
        assert!(!logged_out.contains("second@example.com"));
    }

    #[test]
    fn account_identity_is_sanitized_and_header_is_truncated_at_display_width() {
        let mut state = ready();
        state.auth = AuthState::SignedIn {
            scope: Some(AccountScope::ChatgptEmail(
                "safe\u{1b}[2J\nspoof@example.com".to_owned(),
            )),
        };
        let sanitized = header(&state, 120);
        assert!(sanitized.contains("safe[2J spoof@example.com"));
        assert!(!sanitized.contains('\u{1b}'));

        let long_email = format!("{}@example.com", "account".repeat(20));
        state.auth = AuthState::SignedIn {
            scope: Some(AccountScope::ChatgptEmail(long_email.clone())),
        };
        let narrow = header(&state, 36);
        assert_eq!(UnicodeWidthStr::width(narrow.as_str()), 36);
        assert!(narrow.contains('…'));
        assert!(narrow.ends_with("Context --"));
        assert!(!narrow.contains(&long_email));
        assert!(!narrow.contains("Terminal too small"));
    }

    #[test]
    fn header_right_aligns_context_and_sanitizes_every_dynamic_field() {
        let mut state = ready();
        assert!(header(&state, 100).ends_with("Context --"));

        state.context_remaining_percent = Some(73);
        state.connection = ConnectionState::Failed("e\u{1b}[2J\n界".to_owned());
        state.auth = AuthState::SignedIn {
            scope: Some(AccountScope::ChatgptEmail("u界\n@x".to_owned())),
        };
        state.selected_model = Some("m界\nnext".to_owned());
        state.selected_reasoning = Some("r界\nmax".to_owned());

        let wide = header_text(&state, 100);
        assert_eq!(UnicodeWidthStr::width(wide.as_str()), 100);
        assert!(wide.ends_with("Context 73%"));
        assert!(wide.contains("error: e[2J 界"));
        assert!(wide.contains("u界 @x"));
        assert!(wide.contains("m界 next/r界 max"));
        assert!(!wide.contains('\n'));
        assert!(!wide.contains('\u{1b}'));

        let narrow = header_text(&state, 36);
        assert_eq!(UnicodeWidthStr::width(narrow.as_str()), 36);
        assert!(narrow.contains('…'));
        assert!(narrow.ends_with("Context 73%"));
        assert!(!narrow.contains('\n'));

        let rendered = header(&state, 100);
        assert!(rendered.ends_with("Context 73%"));
    }

    #[test]
    fn renders_ready_streaming_completed_and_error_states() {
        let mut state = ready();
        assert!(screen(&state, &UiState::default(), 100, 20).contains("thread ready"));
        state.transcript.push(TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: "partial reply".to_owned(),
            item_id: None,
            turn_id: None,
        });
        state.turn = TurnState::Streaming {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
        };
        let streaming = screen(&state, &UiState::default(), 100, 20);
        assert!(streaming.contains("streaming"));
        assert!(streaming.contains("partial reply"));
        state.turn = TurnState::Completed {
            turn_id: "turn".to_owned(),
        };
        assert!(screen(&state, &UiState::default(), 100, 20).contains("completed"));
        state.connection = ConnectionState::Failed("upgrade Codex".to_owned());
        state.turn = TurnState::Failed {
            turn_id: None,
            message: "failed".to_owned(),
        };
        let failed = screen(&state, &UiState::default(), 100, 20);
        assert!(failed.contains("upgrade Codex"));
        assert!(failed.contains("failed"));
    }

    #[test]
    fn activity_indicator_is_ephemeral_and_disappears_on_first_text() {
        let mut state = waiting();
        let original_transcript = state.transcript.clone();
        let mut ui = UiState::default();
        assert!(ui.sync_activity_animation(&state));

        let initial = screen(&state, &ui, 70, 16);
        assert!(initial.contains("Agent: ~"));
        assert_eq!(state.transcript, original_transcript);

        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "first text".to_owned(),
        }));
        assert!(ui.sync_activity_animation(&state));

        let with_text = screen(&state, &ui, 70, 16);
        assert!(with_text.contains("first text"));
        assert!(!with_text.contains('~'));
        assert_eq!(state.transcript.len(), original_transcript.len() + 1);
        assert_eq!(state.transcript.last().unwrap().text, "first text");
    }

    #[test]
    fn activity_animation_has_fixed_width_frames_and_bounded_tick_cadence() {
        for frame in ACTIVITY_FRAMES {
            assert!(frame.is_ascii());
            assert_eq!(UnicodeWidthStr::width(frame), 5);
        }

        let mut state = waiting();
        let transcript = state.transcript.clone();
        let mut ui = UiState::default();
        assert!(ui.sync_activity_animation(&state));
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);

        for _ in 1..ACTIVITY_TICKS_PER_FRAME {
            assert!(!ui.advance_activity_animation(&state));
            assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);
        }
        assert!(ui.advance_activity_animation(&state));
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[1]);
        assert!(screen(&state, &ui, 70, 16).contains("Agent: ~~"));
        assert_eq!(state.transcript, transcript);

        state.turn = TurnState::Completed {
            turn_id: "turn".to_owned(),
        };
        assert!(ui.sync_activity_animation(&state));
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);
        assert!(!ui.advance_activity_animation(&state));
        assert!(!ui.advance_activity_animation(&state));
    }

    #[test]
    fn activity_frames_preserve_scrolled_history_and_fit_narrow_terminals() {
        let mut state = waiting();
        state.turn = TurnState::Streaming {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
        };
        state.transcript = (0..24)
            .map(|index| TranscriptEntry {
                role: TranscriptRole::User,
                text: format!("historical message {index}"),
                item_id: None,
                turn_id: None,
            })
            .collect();
        let mut ui = UiState {
            scroll_from_bottom: 5,
            ..UiState::default()
        };
        ui.sync_activity_animation(&state);
        let before = screen(&state, &ui, 50, 12);
        for _ in 0..ACTIVITY_TICKS_PER_FRAME {
            ui.advance_activity_animation(&state);
        }
        let after = screen(&state, &ui, 50, 12);
        assert_eq!(before, after);
        assert_eq!(ui.scroll_from_bottom, 5);

        ui.scroll_from_bottom = 0;
        let narrow = screen(&state, &ui, 36, 9);
        assert!(narrow.contains("Agent:"));
        assert!(narrow.contains('~'));
        assert!(screen(&state, &ui, 35, 8).contains("Terminal too small"));
    }

    #[test]
    fn activity_indicator_stays_in_conversation_when_thinking_panel_is_open() {
        let mut state = waiting();
        state.thinking.visible = true;
        state.context_remaining_percent = Some(73);
        let mut ui = UiState::default();
        assert!(ui.sync_activity_animation(&state));

        let normal = screen(&state, &ui, 100, 20);
        let activity_column = normal
            .lines()
            .find_map(|line| line.find('~'))
            .expect("activity frame should be visible");
        assert!(
            activity_column < 67,
            "activity must stay in the conversation pane"
        );
        assert!(normal.contains("Reasoning"));
        assert!(normal.contains("Awaiting emitted reasoning"));
        assert!(header(&state, 100).ends_with("Context 73%"));

        let narrow = screen(&state, &ui, 36, 12);
        assert!(narrow.contains("Conversation"));
        assert!(narrow.contains("Agent:"));
        assert!(narrow.contains('~'));
        assert!(narrow.contains("Reasoning"));
        assert!(header(&state, 36).ends_with("Context 73%"));
    }

    #[test]
    fn thread_picker_keeps_modal_keys_while_activity_animation_ticks() {
        let mut state = waiting();
        state.context_remaining_percent = Some(100);
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![ThreadChoice {
                id: "thr-old".to_owned(),
                title: "Old conversation".to_owned(),
                updated_at: 1,
            }],
            selected: 0,
            confirmation: None,
            message: None,
        });
        let mut ui = UiState {
            composer: "untouched draft".to_owned(),
            ..UiState::default()
        };
        assert!(ui.sync_activity_animation(&state));
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);

        for _ in 0..ACTIVITY_TICKS_PER_FRAME {
            ui.advance_activity_animation(&state);
        }
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[1]);
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                &state,
            ),
            Some(Intent::ThreadPickerMoveDown)
        );
        assert_eq!(ui.composer, "untouched draft");
        let rendered = screen(&state, &ui, 50, 14);
        assert!(rendered.contains("Saved threads"));
        assert!(header(&state, 50).ends_with("Context 100%"));
    }

    #[test]
    fn handles_small_terminals_and_malicious_control_text() {
        let small = screen(&ready(), &UiState::default(), 24, 6);
        assert!(small.contains("Terminal too small"));
        let mut state = ready();
        state.connection = ConnectionState::Failed("\u{1b}[2Jbad\u{009b}text".to_owned());
        state.transcript.push(TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: "safe\u{1b}[31m\ttext\u{0007}".to_owned(),
            item_id: None,
            turn_id: None,
        });
        let rendered = screen(&state, &UiState::default(), 100, 20);
        assert!(rendered.contains("safe[31m    text"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{009b}'));
        assert_eq!(sanitize_terminal_text("a\r\u{1b}\tb"), "a    b");
    }

    #[test]
    fn composer_wraps_long_wide_input_and_keeps_tail_cursor_visible() {
        let normal_ui = UiState {
            composer: format!("{}{}", "a".repeat(50), "界".repeat(20)),
            ..UiState::default()
        };
        let mut normal = draw(&ready(), &normal_ui, 40, 16);
        assert_eq!(
            normal.backend_mut().get_cursor_position().unwrap(),
            Position::new(15, 13)
        );

        let small_ui = UiState {
            composer: "界".repeat(70),
            ..UiState::default()
        };
        let mut small = draw(&ready(), &small_ui, 36, 9);
        assert_eq!(
            small.backend_mut().get_cursor_position().unwrap(),
            Position::new(5, 6)
        );
        let rendered = (0..9)
            .map(|y| {
                (0..36)
                    .map(|x| small.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains('界'));
    }

    #[test]
    fn wraps_long_catalog_notices_and_shows_actual_turn_failure() {
        let mut state = ready();
        state.notice = Some(
            "Available models: model-alpha, model-beta, model-gamma, model-delta, \
             model-epsilon, model-zeta, model-eta, model-theta, model-iota, model-kappa"
                .to_owned(),
        );
        let catalog = screen(&state, &UiState::default(), 50, 18);
        assert!(catalog.contains("Available models"));
        assert!(catalog.contains("model-kappa"));

        state.notice = None;
        state.turn = TurnState::Failed {
            turn_id: Some("turn".to_owned()),
            message: "The selected model\u{1b}[31m rejected this request; choose /model and retry."
                .to_owned(),
        };
        let failure = screen(&state, &UiState::default(), 54, 18);
        assert!(failure.contains("Turn failed"));
        assert!(failure.contains("selected model[31m rejected"));
        assert!(failure.contains("/model and retry."));
        assert!(!failure.contains('\u{1b}'));
    }

    #[test]
    fn thinking_panel_renders_closed_open_narrow_streaming_and_error_states() {
        let mut state = ready();
        state.context_remaining_percent = Some(73);
        let closed = screen(&state, &UiState::default(), 100, 20);
        assert!(!closed.contains("Only reasoning content"));
        assert!(header(&state, 100).ends_with("Context 73%"));

        state.thinking.visible = true;
        state.turn = TurnState::Streaming {
            thread_id: "thread".to_owned(),
            turn_id: "turn".to_owned(),
        };
        let awaiting = screen(&state, &UiState::default(), 100, 20);
        assert!(awaiting.contains("Reasoning"));
        assert!(awaiting.contains("Awaiting emitted reasoning"));

        state.thinking.entries.push(ThinkingEntry {
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            text: "Checking facts safely".to_owned(),
            completed: false,
        });
        let normal = screen(&state, &UiState::default(), 100, 20);
        assert!(normal.contains("Only reasoning content"));
        assert!(normal.contains("Summary:"));
        assert!(normal.contains("Checking facts safely"));
        assert!(header(&state, 100).ends_with("Context 73%"));

        let narrow = screen(&state, &UiState::default(), 52, 16);
        assert!(narrow.contains("Conversation"));
        assert!(narrow.contains("Reasoning"));
        assert!(narrow.contains("Summary:"));
        assert!(narrow.contains("Message"));

        let minimum_width = screen(&state, &UiState::default(), 36, 12);
        assert!(minimum_width.contains("Conversation"));
        assert!(minimum_width.contains("Reasoning"));
        assert!(minimum_width.contains("Message"));
        assert!(header(&state, 36).ends_with("Context 73%"));

        state.turn = TurnState::Failed {
            turn_id: Some("turn".to_owned()),
            message: "model failed".to_owned(),
        };
        let failed = screen(&state, &UiState::default(), 100, 20);
        assert!(failed.contains("Turn failed."));
        assert!(failed.contains("Checking facts safely"));

        state.thinking.entries.clear();
        let empty_failure = screen(&state, &UiState::default(), 100, 20);
        assert!(empty_failure.contains("No reasoning content"));
        assert!(empty_failure.contains("before this turn"));
    }

    #[test]
    fn input_supports_multiline_help_interrupt_scroll_and_quit() {
        let mut ui = UiState::default();
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::NONE,
        )));
        ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
        assert_eq!(ui.composer, "h\n");
        ui.composer = "/help".to_owned();
        assert!(ui
            .handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
            .is_none());
        assert!(ui.overlay.is_some());
        assert!(ui
            .handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            .is_none());
        assert!(matches!(
            ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            Some(crate::app::Intent::Interrupt)
        ));
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::PageUp,
            KeyModifiers::NONE,
        )));
        assert_eq!(ui.scroll_from_bottom, 1);
        assert!(matches!(
            ui.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))),
            Some(crate::app::Intent::Quit)
        ));
    }

    #[test]
    fn thread_picker_renders_at_normal_and_narrow_supported_widths() {
        let mut state = ready();
        state.preferences.thread_id = Some("thr-active".to_owned());
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![
                ThreadChoice {
                    id: "thr-active".to_owned(),
                    title: "Current conversation".to_owned(),
                    updated_at: 20,
                },
                ThreadChoice {
                    id: "thr-old".to_owned(),
                    title: "An older conversation".to_owned(),
                    updated_at: 10,
                },
            ],
            selected: 1,
            confirmation: None,
            message: None,
        });
        let normal = screen(&state, &UiState::default(), 90, 24);
        assert!(normal.contains("Saved threads"));
        assert!(normal.contains("Current conversation"));
        assert!(normal.contains("ACTIVE"));
        assert!(normal.contains("D clear inactive"));

        let narrow = screen(&state, &UiState::default(), 36, 9);
        assert!(narrow.contains("Saved threads"));
        assert!(narrow.contains("Current conversation") || narrow.contains("An older"));
    }

    #[test]
    fn thread_picker_keys_are_modal_and_confirmation_is_a_second_action() {
        let mut state = ready();
        state.thinking.visible = true;
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![ThreadChoice {
                id: "thr-old".to_owned(),
                title: "Old".to_owned(),
                updated_at: 1,
            }],
            selected: 0,
            confirmation: None,
            message: None,
        });
        let mut ui = UiState {
            composer: "draft message".to_owned(),
            ..UiState::default()
        };
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                &state,
            ),
            Some(Intent::ThreadPickerMoveDown)
        );
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
                &state,
            ),
            Some(Intent::ThreadPickerRequestDelete)
        );
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)),
                &state,
            ),
            Some(Intent::ThreadPickerRequestClearInactive)
        );
        assert_eq!(ui.composer, "draft message");

        let combined = screen(&state, &ui, 36, 12);
        assert!(combined.contains("Saved threads"));
        assert!(!combined.contains("draft message"));

        state.thread_picker.as_mut().unwrap().confirmation =
            Some(ThreadDeleteConfirmation::Selected {
                target: ThreadChoice {
                    id: "thr-old".to_owned(),
                    title: "Old".to_owned(),
                    updated_at: 1,
                },
            });
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &state,
            ),
            Some(Intent::ThreadPickerConfirmDelete)
        );
        assert_eq!(
            ui.handle_event_for_state(
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                &state,
            ),
            Some(Intent::ThreadPickerCancelDelete)
        );
        let confirmation = screen(&state, &ui, 70, 20);
        assert!(confirmation.contains("Confirm permanent deletion"));
        assert!(confirmation.contains("thr-old"));
        assert!(confirmation.contains("cannot be undone"));
    }
}
