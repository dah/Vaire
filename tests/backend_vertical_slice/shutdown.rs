use super::support::*;

#[tokio::test]
async fn shutdown_reaps_the_child_even_when_persistence_fails() {
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
IFS= read -r hold
"#
    );
    let paths = IsolationPaths::prepare(temp.path().join("runtime")).unwrap();
    let executable = script(temp.path(), &body);
    let transport = AppServerTransport::spawn(ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: temp.path().to_owned(),
        env: Vec::new(),
    })
    .await
    .unwrap();
    let pid = transport.child_pid();
    let session = SessionService::new(transport, paths, FullAccessPolicy);
    let mut backend =
        BackendCoordinator::new(session, FailingPreferences, RecordingBrowser::default());
    backend.startup().await.unwrap();

    assert!(backend.shutdown().await.is_err());
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success(),
        "app-server child was not reaped after a persistence failure"
    );
}
