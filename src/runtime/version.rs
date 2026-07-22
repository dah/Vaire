use super::*;

pub const TESTED_CODEX_VERSION: &str = "0.144.6";
const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;

pub fn resolve_codex(override_name: Option<&OsStr>) -> Result<PathBuf, RuntimeError> {
    let name = override_name.unwrap_or_else(|| OsStr::new("codex"));
    let candidate = PathBuf::from(name);
    if candidate.components().count() > 1 {
        return canonical_executable(&candidate).ok_or(RuntimeError::CodexNotFound);
    }
    let path = std::env::var_os("PATH").ok_or(RuntimeError::CodexNotFound)?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find_map(|candidate| canonical_executable(&candidate))
        .ok_or(RuntimeError::CodexNotFound)
}

fn canonical_executable(path: &Path) -> Option<PathBuf> {
    is_executable(path)
        .then(|| fs::canonicalize(path).ok())
        .flatten()
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(in crate::runtime) async fn verify_codex_version(
    executable: &Path,
    codex_home: &Path,
) -> Result<(), RuntimeError> {
    verify_codex_version_with_timeout(executable, codex_home, Duration::from_secs(3)).await
}

pub(in crate::runtime) async fn verify_codex_version_with_timeout(
    executable: &Path,
    codex_home: &Path,
    timeout: Duration,
) -> Result<(), RuntimeError> {
    let mut command = Command::new(executable);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CODEX_") {
            command.env_remove(key);
        }
    }
    command
        .kill_on_drop(true)
        .env("CODEX_HOME", codex_home)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        RuntimeError::VersionCheck("version command stdout was unavailable".to_owned())
    })?;
    let (status, stdout) = collect_version_output(child, stdout, timeout).await?;
    if !status.success() {
        return Err(RuntimeError::VersionCheck(format!(
            "version command exited with {}",
            status
        )));
    }
    let stdout = String::from_utf8_lossy(&stdout);
    let version = find_version(&stdout)
        .ok_or_else(|| RuntimeError::VersionCheck("unrecognized version output".to_owned()))?;
    let minimum = parse_version(TESTED_CODEX_VERSION).expect("tested version is valid");
    if version < minimum {
        return Err(RuntimeError::UnsupportedVersion(format!(
            "{}.{}.{}",
            version.0, version.1, version.2
        )));
    }
    Ok(())
}

pub(in crate::runtime) async fn collect_version_output(
    mut child: Child,
    mut stdout: ChildStdout,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>), RuntimeError> {
    let probe = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((MAX_VERSION_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
        if bytes.len() > MAX_VERSION_OUTPUT_BYTES {
            return Err(RuntimeError::VersionCheck(
                "version command output exceeded safe limit".to_owned(),
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|error| RuntimeError::VersionCheck(error.to_string()))?;
        Ok((status, bytes))
    };
    let outcome = time::timeout(timeout, probe).await;
    match outcome {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => {
            terminate_version_child(&mut child).await;
            Err(error)
        }
        Err(_) => {
            terminate_version_child(&mut child).await;
            Err(RuntimeError::VersionCheck(
                "version command timed out".to_owned(),
            ))
        }
    }
}

pub(in crate::runtime) async fn terminate_version_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.start_kill();
    }
    let _ = time::timeout(Duration::from_secs(1), child.wait()).await;
}

pub(in crate::runtime) fn find_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut words = value.split_whitespace();
    while let Some(word) = words.next() {
        if word == "codex-cli" {
            return words
                .next()
                .and_then(|version| parse_version(version.trim_start_matches('v')));
        }
    }
    None
}

pub(in crate::runtime) fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.split(['-', '+']).next()?;
    let mut parts = value.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}
