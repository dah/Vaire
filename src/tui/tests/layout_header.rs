use super::*;

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
    state.selected_model = Some(crate::provider::ModelKey {
        provider: crate::provider::ProviderId::Codex,
        id: "m界\nnext".to_owned(),
    });
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
fn handles_small_terminals_and_malicious_control_text() {
    for (width, height) in [(0, 0), (0, 9), (36, 0), (1, 1), (35, 8)] {
        let _ = draw(&ready(), &UiState::default(), width, height);
    }
    let small = screen(&ready(), &UiState::default(), 24, 6);
    assert!(small.contains("Terminal too small"));
    let mut state = ready();
    state.connection = ConnectionState::Failed("\u{1b}[2Jbad\u{009b}text".to_owned());
    state.transcript.push(TranscriptEntry {
        provider: crate::provider::ProviderId::Codex,
        role: TranscriptRole::Assistant,
        status: TranscriptEntryStatus::Normal,
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
