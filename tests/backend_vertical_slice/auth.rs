use super::support::*;

#[tokio::test]
async fn chatgpt_browser_login_completion_and_logout_are_protocol_driven() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-1","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-other","success":true,"error":null}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-1","success":true,"error":null}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":5,"result":{{"account":{{"type":"chatgpt","email":"user@example.com","planType":"plus"}},"requiresOpenaiAuth":true}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-1","success":true,"error":null}}}}'
IFS= read -r logout
printf '%s\n' '{{"id":6,"result":{{}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV3::default()),
        browser,
    );
    backend.startup().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.handle_intent(Intent::Login).await.unwrap();
    assert_eq!(opened.lock().unwrap().len(), 1);
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/login device")));
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SigningIn { .. }));
    backend.pump_event().await.unwrap();
    assert_eq!(
        backend.state().auth,
        AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("user@example.com"),
        }
    );
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("Signed in to ChatGPT")
    );
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedIn { .. }));
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("Signed in to ChatGPT")
    );
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn idless_login_failure_allows_retry_and_pending_login_can_be_cancelled() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r first_login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-failed","authUrl":"https://auth.openai.com/oauth?state=failed"}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"success":false,"error":"browser sign-in failed"}}}}'
IFS= read -r second_login
printf '%s\n' '{{"id":5,"result":{{"type":"chatgpt","loginId":"login-cancel","authUrl":"https://auth.openai.com/oauth?state=cancel"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-cancel"'*) ;;
  *) exit 90 ;;
esac
printf '%s\n' '{{"id":6,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":7,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r third_login
printf '%s\n' '{{"id":8,"result":{{"type":"chatgpt","loginId":"login-retry","authUrl":"https://auth.openai.com/oauth?state=retry"}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV3::default()),
        browser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-failed"
    ));

    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert_eq!(
        backend.state().notice.as_deref(),
        Some("browser sign-in failed")
    );

    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-cancel"
    ));
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));

    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-retry"
    ));
    assert_eq!(opened.lock().unwrap().len(), 3);
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn device_code_login_opens_verification_page_displays_code_and_can_be_cancelled() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r device_login
case "$device_login" in
  *'"method":"account/login/start"'*'"type":"chatgptDeviceCode"'*) ;;
  *) exit 91 ;;
esac
printf '%s\n' '{{"id":4,"result":{{"type":"chatgptDeviceCode","loginId":"login-device","userCode":"ABCD-EFGH","verificationUrl":"https://auth.openai.com/codex/device"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-device"'*) ;;
  *) exit 92 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":6,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
printf '%s\n' '{{"method":"account/login/completed","params":{{"loginId":"login-device","success":false,"error":"cancelled"}}}}'
IFS= read -r hold
"#
    );
    let browser = RecordingBrowser::default();
    let opened = browser.urls.clone();
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV3::default()),
        browser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::LoginDevice).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-device"
    ));
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://auth.openai.com/codex/device"]
    );
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("ABCD-EFGH")));

    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("cancelled")));
    backend.pump_event().await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("cancelled")));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn browser_open_failure_retains_login_id_until_explicit_cancellation() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-browser-failed","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
IFS= read -r cancel_login
case "$cancel_login" in
  *'"method":"account/login/cancel"'*'"loginId":"login-browser-failed"'*) ;;
  *) exit 93 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"status":"canceled"}}}}'
IFS= read -r refreshed_account
printf '%s\n' '{{"id":6,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV3::default()),
        FailingBrowser,
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-browser-failed"
    ));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/logout")));

    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(backend.state().auth, AuthState::SignedOut));
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_failure_keeps_pending_login_retryable() {
    let temp = tempdir().unwrap();
    let body = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{INITIALIZED}'
IFS= read -r initialized
IFS= read -r account
printf '%s\n' '{{"id":2,"result":{{"account":null,"requiresOpenaiAuth":true}}}}'
IFS= read -r models
printf '%s\n' '{MODEL_PAGE}'
IFS= read -r login
printf '%s\n' '{{"id":4,"result":{{"type":"chatgpt","loginId":"login-cancel-retry","authUrl":"https://auth.openai.com/oauth?state=opaque"}}}}'
IFS= read -r cancel_login
printf '%s\n' '{{"id":5,"error":{{"code":-32000,"message":"temporary cancel failure"}}}}'
IFS= read -r hold
"#
    );
    let mut backend = BackendCoordinator::new(
        session(temp.path(), &body).await,
        MemoryPreferences::new(PreferencesV3::default()),
        RecordingBrowser::default(),
    );

    backend.startup().await.unwrap();
    backend.handle_intent(Intent::Login).await.unwrap();
    backend.handle_intent(Intent::Logout).await.unwrap();
    assert!(matches!(
        backend.state().auth,
        AuthState::SigningIn { ref login_id } if login_id == "login-cancel-retry"
    ));
    assert!(backend
        .state()
        .notice
        .as_deref()
        .is_some_and(|notice| notice.contains("/logout to retry")));
    backend.shutdown().await.unwrap();
}
