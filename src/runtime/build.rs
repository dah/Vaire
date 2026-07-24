use super::*;

pub(in crate::runtime) async fn build_backend(
    config: RuntimeConfig,
) -> Result<BackendCoordinator<FilePreferences, MacOsBrowser>, RuntimeError> {
    fs::create_dir_all(&config.paths.support_dir).map_err(RuntimeError::Paths)?;
    fs::set_permissions(&config.paths.support_dir, fs::Permissions::from_mode(0o700))
        .map_err(RuntimeError::Paths)?;
    let diagnostics_path = config.paths.diagnostics_dir.join("vaire.log");
    let diagnostics: Arc<dyn DiagnosticSink> =
        Arc::new(FileDiagnosticSink::create(&diagnostics_path).map_err(RuntimeError::Paths)?);
    let openrouter = (|| -> Result<OpenRouterService, RuntimeError> {
        let credentials: Arc<dyn CredentialStore> = Arc::new(
            FileCredentialStore::new(
                CredentialAccount::OpenRouterApiKey,
                &config.paths.openrouter_home_dir,
                &config.paths.openrouter_credential_file,
            )
            .map_err(|error| RuntimeError::OpenRouter(error.to_string()))?,
        );
        let openrouter_store: Arc<dyn OpenRouterConversationStore> = Arc::new(
            FileOpenRouterStore::new(&config.paths.openrouter_dir)
                .map_err(|error| RuntimeError::OpenRouter(error.to_string()))?,
        );
        let client = OpenRouterClient::production(credentials.clone())
            .map_err(|error| RuntimeError::OpenRouter(error.to_string()))?;
        Ok(OpenRouterService::new(
            client,
            credentials,
            openrouter_store,
        ))
    })();

    let claude = async {
        prepare_private_directory(&config.paths.claude_cli_home_dir)?;
        prepare_private_directory(&config.paths.claude_conversation_dir)?;
        let credentials: Arc<dyn CredentialStore> = Arc::new(
            FileCredentialStore::new(
                CredentialAccount::AnthropicConsoleApiKey,
                &config.paths.anthropic_credential_home_dir,
                &config.paths.anthropic_credential_file,
            )
            .map_err(|error| RuntimeError::Claude(error.to_string()))?,
        );
        let store: Arc<dyn ClaudeSessionStore> = Arc::new(
            FileClaudeSessionStore::new(&config.paths.claude_store_dir)
                .map_err(|error| RuntimeError::Claude(error.to_string()))?,
        );
        let executable = resolve_claude(config.claude_override.as_deref())
            .map_err(|error| RuntimeError::Claude(error.to_string()))?;
        verify_claude_version(
            &executable,
            &config.paths.claude_cli_home_dir,
            Duration::from_secs(3),
        )
        .await
        .map_err(|error| RuntimeError::Claude(error.to_string()))?;
        let policy = ClaudeCliPolicy::new(
            executable,
            config.paths.claude_cli_home_dir.clone(),
            config.paths.claude_conversation_dir.clone(),
        );
        let service = ClaudeService::new(policy.clone(), credentials.clone(), store);
        Ok::<_, RuntimeError>(ClaudeBackendRuntime::new(service, credentials, policy))
    }
    .await;

    let codex = async {
        let isolation = IsolationPaths::prepare(&config.paths.runtime_dir)
            .map_err(RuntimeError::Paths)?
            .with_historical_conversation(config.paths.historical_conversation_dir.clone());
        let executable = resolve_codex(config.codex_override.as_deref())?;
        verify_codex_version(&executable, &isolation.codex_home).await?;
        let spec = ProcessSpec::codex(executable, &isolation, &FullAccessPolicy);
        let transport = AppServerTransport::spawn_with_diagnostics(spec, diagnostics)
            .await
            .map_err(|error| RuntimeError::AppServer(error.to_string()))?;
        Ok::<_, RuntimeError>(SessionService::new(transport, isolation, FullAccessPolicy))
    }
    .await;
    let preferences = FilePreferences::new(config.paths.preferences_file);
    let mut backend = match codex {
        Ok(session) => BackendCoordinator::new(session, preferences, MacOsBrowser),
        Err(error) => {
            BackendCoordinator::without_codex(preferences, MacOsBrowser, error.to_string())
        }
    };
    match openrouter {
        Ok(openrouter) => backend = backend.with_openrouter(openrouter),
        Err(_) => backend.record_openrouter_unavailable(),
    }
    match claude {
        Ok(claude) => backend = backend.with_claude(claude),
        Err(error) => backend.record_claude_unavailable(error.to_string()),
    }
    Ok(backend)
}

fn prepare_private_directory(path: &Path) -> Result<(), RuntimeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            // SAFETY: geteuid has no preconditions and retains no pointers.
            let owner = unsafe { libc::geteuid() };
            if !metadata.file_type().is_dir()
                || metadata.uid() != owner
                || metadata.permissions().mode() & 0o7777 != 0o700
            {
                return Err(RuntimeError::Claude(
                    "Claude runtime directory is not an owner-only directory".to_owned(),
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(RuntimeError::Paths)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(RuntimeError::Paths)
        }
        Err(error) => Err(RuntimeError::Paths(error)),
    }
}

pub(in crate::runtime) fn publish(states: &watch::Sender<AppState>, state: &AppState) {
    states.send_replace(state.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_isolation_failure_does_not_prevent_openrouter_startup() {
        let directory = tempfile::tempdir().unwrap();
        let mut paths = AppPaths::from_home(directory.path());
        fs::create_dir_all(&paths.support_dir).unwrap();
        paths.runtime_dir = paths.support_dir.join("runtime-blocker");
        fs::write(&paths.runtime_dir, b"not a directory").unwrap();

        let mut backend = build_backend(RuntimeConfig {
            paths,
            codex_override: None,
            claude_override: None,
        })
        .await
        .expect("shared paths and OpenRouter construction should remain usable");
        backend.startup().await.unwrap();

        assert!(matches!(
            backend.state().connection,
            crate::app::ConnectionState::Failed(_)
        ));
        assert_eq!(
            backend.state().openrouter.auth,
            crate::openrouter::OpenRouterAuthStatus::Missing
        );
    }
}
