use super::*;

pub(in crate::tui) fn render_popup(
    frame: &mut Frame<'_>,
    area: Rect,
    popup: &PopupState,
    state: &AppState,
    secret_mask: Option<&str>,
) {
    if let PopupState::Conversation(picker) = popup {
        render_thread_picker(
            frame,
            area,
            picker,
            state.active_provider,
            state.active_saved_thread_id(),
        );
        return;
    }
    let width = area.width.saturating_sub(4).clamp(32, 76);
    let height = area.height.saturating_sub(4).clamp(7, 20);
    let popup_area = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup_area);
    match popup {
        PopupState::Conversation(_) => unreachable!("conversation popup returned above"),
        PopupState::Auth { mode, selected } => {
            let title = match mode {
                AuthPopupMode::Login => " Sign in provider ",
                AuthPopupMode::Logout => " Sign out provider ",
            };
            let codex = format!(
                "{} Codex       {}",
                if *selected == crate::provider::ProviderId::Codex {
                    ">"
                } else {
                    " "
                },
                codex_auth_label(state)
            );
            let openrouter = format!(
                "{} OpenRouter  {:?}",
                if *selected == crate::provider::ProviderId::OpenRouter {
                    ">"
                } else {
                    " "
                },
                state.openrouter.auth
            );
            let claude = format!(
                "{} Claude      {}",
                if *selected == crate::provider::ProviderId::Claude {
                    ">"
                } else {
                    " "
                },
                claude_auth_label(state)
            );
            let footer = match mode {
                AuthPopupMode::Login => {
                    "Enter choose • Claude opens native browser login • d Codex device • c OpenRouter models • r refresh • Esc close"
                }
                AuthPopupMode::Logout => {
                    "Enter sign out selected provider • Claude also signs out system Claude Code • Esc close"
                }
            };
            frame.render_widget(
                Paragraph::new(format!("{codex}\n{openrouter}\n{claude}\n\n{footer}"))
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .wrap(Wrap { trim: true }),
                popup_area,
            );
        }
        PopupState::ProviderSecret { provider } => {
            let openrouter_editor = *provider == crate::provider::ProviderId::OpenRouter;
            let validating = openrouter_editor
                && !matches!(
                    state.openrouter.credential_validation,
                    OpenRouterCredentialValidation::Idle
                );
            let (title, body) = if !openrouter_editor {
                (
                    " Credential unavailable ",
                    "Only OpenRouter accepts a key in Vairë. Close this popup and use the provider login flow."
                        .to_owned(),
                )
            } else if validating {
                (
                    " OpenRouter credential ",
                    "Validating OpenRouter credential…".to_owned(),
                )
            } else {
                (
                    " OpenRouter credential ",
                    format!(
                        "API key: {}\n\nEnter validate and save • Ctrl-U clear • Esc cancel",
                        secret_mask.unwrap_or("••••••••")
                    ),
                )
            };
            frame.render_widget(
                Paragraph::new(body)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .wrap(Wrap { trim: true }),
                popup_area,
            );
        }
        PopupState::Model {
            choices,
            selected,
            search,
        } => {
            let count = choices
                .iter()
                .filter(|key| model_search_matches(key, search))
                .count();
            let visible_rows = usize::from(popup_area.height.saturating_sub(6)).max(1);
            let start = centered_start(count, *selected, visible_rows);
            let rows = choices
                .iter()
                .filter(|key| model_search_matches(key, search))
                .skip(start)
                .take(visible_rows)
                .enumerate()
                .map(|(index, key)| {
                    let index = start + index;
                    format!(
                        "{} [{}] {}",
                        if index == *selected { ">" } else { " " },
                        provider_label(key.provider),
                        key.id
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(
                Paragraph::new(format!(
                    "Search: {search}\n{rows}\n\nSwitching provider or changing a Claude alias starts a new conversation; use /resume for history."
                ))
                .block(Block::default().title(" Models ").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
                popup_area,
            );
        }
        PopupState::OpenRouterCatalog {
            models,
            draft_enabled,
            selected,
            search,
        } => {
            let count = models
                .iter()
                .filter(|model| catalog_search_matches(model, search))
                .count();
            let visible_rows = usize::from(popup_area.height.saturating_sub(6)).max(1);
            let start = centered_start(count, *selected, visible_rows);
            let rows = models
                .iter()
                .filter(|model| catalog_search_matches(model, search))
                .skip(start)
                .take(visible_rows)
                .enumerate()
                .map(|(index, model)| {
                    let index = start + index;
                    format!(
                        "{} [{}] {}",
                        if index == *selected { ">" } else { " " },
                        if draft_enabled.contains(&model.id) {
                            "x"
                        } else {
                            " "
                        },
                        model.name.as_deref().unwrap_or(&model.id)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(
                Paragraph::new(format!(
                    "Search: {search}\n{rows}\n\nSpace toggle • Enter save • Esc discard"
                ))
                .block(
                    Block::default()
                        .title(" OpenRouter models ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false }),
                popup_area,
            );
        }
    }
}

fn centered_start(count: usize, selected: usize, visible_rows: usize) -> usize {
    selected
        .saturating_sub(visible_rows / 2)
        .min(count.saturating_sub(visible_rows))
}

fn provider_label(provider: crate::provider::ProviderId) -> &'static str {
    match provider {
        crate::provider::ProviderId::Codex => "Codex",
        crate::provider::ProviderId::OpenRouter => "OpenRouter",
        crate::provider::ProviderId::Claude => "Claude",
    }
}

fn claude_auth_label(state: &AppState) -> &'static str {
    if matches!(
        state.claude.availability,
        crate::app::ClaudeAvailability::Unavailable(_)
    ) {
        return "CLI unavailable";
    }
    match &state.claude.auth_operation {
        crate::app::ClaudeAuthOperation::Checking { .. } => "checking status",
        crate::app::ClaudeAuthOperation::AwaitingTerminal { request } => match request.action {
            crate::claude::ClaudeAuthAction::Login => "signing in via browser",
            crate::claude::ClaudeAuthAction::Logout => "signing out",
        },
        crate::app::ClaudeAuthOperation::Idle => match state.claude.auth {
            crate::claude::ClaudeAuthStatus::SignedOut => "signed out",
            crate::claude::ClaudeAuthStatus::Subscription => "subscription connected",
            crate::claude::ClaudeAuthStatus::Unsupported => "unsupported auth",
            crate::claude::ClaudeAuthStatus::Unverified => "auth unverified",
            crate::claude::ClaudeAuthStatus::CliUnavailable => "CLI unavailable",
        },
    }
}

fn codex_auth_label(state: &AppState) -> &'static str {
    match state.auth {
        AuthState::Unknown => "unknown",
        AuthState::SignedOut => "signed out",
        AuthState::SigningIn { .. } => "signing in",
        AuthState::SignedIn { .. } => "signed in",
        AuthState::Unsupported(_) => "unsupported",
    }
}
