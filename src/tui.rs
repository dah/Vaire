use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{
        AppState, AuthState, ConnectionState, Intent, ThinkingKind, ThreadDeleteConfirmation,
        ThreadPickerPhase, ThreadPickerState, ThreadState, TranscriptRole, TurnState,
    },
    command::{parse, HELP_TEXT},
    text::append_sanitized_terminal_text,
};

const MIN_WIDTH: u16 = 36;
const MIN_HEIGHT: u16 = 9;
const MAX_COMPOSER_ROWS: usize = 5;
const MAX_MESSAGE_ROWS: usize = 4;

mod conversation;
mod display;
mod header_message;
mod layout;
mod state;
mod thread_picker;

pub use crate::text::sanitize_terminal_text;
pub(in crate::tui) use conversation::*;
pub(in crate::tui) use display::*;
pub(in crate::tui) use header_message::*;
pub use layout::render;
pub use state::UiState;
#[cfg(test)]
pub(in crate::tui) use state::{ACTIVITY_FRAMES, ACTIVITY_TICKS_PER_FRAME, MAX_COMPOSER_BYTES};
pub(in crate::tui) use thread_picker::render_thread_picker;

#[cfg(test)]
mod tests;
