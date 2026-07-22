use super::*;

#[test]
fn parses_tested_cli_version_output() {
    assert_eq!(find_version("codex-cli 0.144.6\n"), Some((0, 144, 6)));
    assert_eq!(find_version("codex-cli v0.145.0-beta"), Some((0, 145, 0)));
    assert!(find_version("codex-cli 0.144.5").unwrap() < (0, 144, 6));
    assert_eq!(
        find_version("dependency 999.0.0\ncodex-cli 0.144.5\n"),
        Some((0, 144, 5))
    );
    assert_eq!(find_version("999.0.0"), None);
    assert_eq!(find_version("not-a-version"), None);
}

#[test]
fn explicit_executable_path_must_exist_and_be_executable() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex");
    fs::write(&path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        resolve_codex(Some(path.as_os_str())).unwrap(),
        fs::canonicalize(&path).unwrap()
    );
    assert!(matches!(
        resolve_codex(Some(temp.path().join("missing").as_os_str())),
        Err(RuntimeError::CodexNotFound)
    ));
}

#[test]
fn relative_executable_path_is_made_absolute_before_the_child_changes_cwd() {
    let temp = tempfile::tempdir_in("target").unwrap();
    let path = temp.path().join("codex");
    fs::write(&path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let resolved = resolve_codex(Some(path.as_os_str())).unwrap();
    assert!(resolved.is_absolute());
    assert_eq!(resolved, fs::canonicalize(path).unwrap());
}

#[tokio::test]
async fn version_probe_uses_the_dedicated_codex_home() {
    let temp = tempdir().unwrap();
    let executable = temp.path().join("codex");
    fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s' \"$CODEX_HOME\" > \"$CODEX_HOME/probed\"\nprintf 'codex-cli 0.144.6\\n'\n",
        )
        .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let codex_home = temp.path().join("dedicated");
    fs::create_dir(&codex_home).unwrap();
    verify_codex_version(&executable, &codex_home)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(codex_home.join("probed")).unwrap(),
        codex_home.to_string_lossy()
    );
}

#[tokio::test]
async fn timed_out_version_probe_kills_and_reaps_an_already_started_child() {
    let temp = tempdir().unwrap();
    let executable = temp.path().join("codex");
    fs::write(&executable, "#!/bin/sh\nexec sleep 10\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = tokio::process::Command::new(&executable);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().unwrap();
    let pid = child.id().expect("spawned child has a process id");
    let stdout = child.stdout.take().unwrap();

    let error = collect_version_output(child, stdout, std::time::Duration::from_millis(20))
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::VersionCheck(_)));
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "timed-out version child still exists");
}

#[tokio::test]
async fn version_probe_rejects_resource_exhausting_output() {
    let temp = tempdir().unwrap();
    let executable = temp.path().join("codex");
    fs::write(
        &executable,
        "#!/bin/sh\nhead -c 131072 /dev/zero\nprintf 'codex-cli 0.144.6\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let codex_home = temp.path().join("dedicated");
    fs::create_dir(&codex_home).unwrap();

    let error = verify_codex_version_with_timeout(
        &executable,
        &codex_home,
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        RuntimeError::VersionCheck(message) if message.contains("exceeded safe limit")
    ));
}
