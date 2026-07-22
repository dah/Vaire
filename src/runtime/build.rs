use super::*;

pub(in crate::runtime) async fn build_backend(
    config: RuntimeConfig,
) -> Result<BackendCoordinator<FilePreferences, MacOsBrowser>, RuntimeError> {
    fs::create_dir_all(&config.paths.support_dir).map_err(RuntimeError::Paths)?;
    fs::set_permissions(&config.paths.support_dir, fs::Permissions::from_mode(0o700))
        .map_err(RuntimeError::Paths)?;
    let isolation =
        IsolationPaths::prepare(&config.paths.runtime_dir).map_err(RuntimeError::Paths)?;
    let diagnostics_path = config.paths.diagnostics_dir.join("agentharness.log");
    let diagnostics: Arc<dyn DiagnosticSink> =
        Arc::new(FileDiagnosticSink::create(&diagnostics_path).map_err(RuntimeError::Paths)?);
    let executable = resolve_codex(config.codex_override.as_deref())?;
    verify_codex_version(&executable, &isolation.codex_home).await?;
    let spec = ProcessSpec::codex(executable, &isolation, &FullAccessPolicy);
    let transport = AppServerTransport::spawn_with_diagnostics(spec, diagnostics)
        .await
        .map_err(|error| RuntimeError::AppServer(error.to_string()))?;
    let session = SessionService::new(transport, isolation, FullAccessPolicy);
    Ok(BackendCoordinator::new(
        session,
        FilePreferences::new(config.paths.preferences_file),
        MacOsBrowser,
    ))
}

pub(in crate::runtime) fn publish(states: &watch::Sender<AppState>, state: &AppState) {
    states.send_replace(state.clone());
}
