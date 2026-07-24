use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;
use url::Url;

mod migration;

pub(crate) use migration::{legacy_support_dir, migrate_support_root};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub support_dir: PathBuf,
    pub preferences_file: PathBuf,
    pub runtime_dir: PathBuf,
    pub openrouter_dir: PathBuf,
    pub openrouter_home_dir: PathBuf,
    pub openrouter_credential_file: PathBuf,
    pub claude_cli_home_dir: PathBuf,
    pub claude_conversation_dir: PathBuf,
    pub claude_probe_dir: PathBuf,
    pub anthropic_credential_home_dir: PathBuf,
    pub anthropic_credential_file: PathBuf,
    pub claude_store_dir: PathBuf,
    pub diagnostics_dir: PathBuf,
    pub(crate) historical_conversation_dir: Option<PathBuf>,
}

impl AppPaths {
    pub fn from_home(home: &Path) -> Self {
        #[cfg(target_os = "macos")]
        let support_dir = home
            .join("Library")
            .join("Application Support")
            .join("vaire");

        #[cfg(target_os = "macos")]
        let historical_conversation_dir = Some(
            legacy_support_dir(home)
                .join("runtime")
                .join("conversation"),
        );

        #[cfg(all(unix, not(target_os = "macos")))]
        let support_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("vaire");

        #[cfg(all(unix, not(target_os = "macos")))]
        let historical_conversation_dir = None;

        let runtime_dir = support_dir.join("runtime");
        let openrouter_home_dir = runtime_dir.join("openrouter-home");
        let anthropic_credential_home_dir = runtime_dir.join("anthropic-home");
        Self {
            preferences_file: support_dir.join("preferences.json"),
            runtime_dir: runtime_dir.clone(),
            openrouter_dir: support_dir.join("openrouter"),
            openrouter_credential_file: openrouter_home_dir.join("api-key"),
            openrouter_home_dir,
            claude_cli_home_dir: runtime_dir.join("claude-home"),
            claude_conversation_dir: runtime_dir.join("claude-conversation"),
            claude_probe_dir: runtime_dir.join("claude-probes"),
            anthropic_credential_file: anthropic_credential_home_dir.join("api-key"),
            anthropic_credential_home_dir,
            claude_store_dir: support_dir.join("claude"),
            diagnostics_dir: support_dir.join("diagnostics"),
            historical_conversation_dir,
            support_dir,
        }
    }

    pub fn discover() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        Ok(Self::from_home(&home))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BrowserError {
    #[error("login URL must be a safe HTTPS URL")]
    UnsafeUrl,
    #[error("could not open the login browser: {0}")]
    Open(String),
}

pub fn validate_login_url(value: &str) -> Result<Url, BrowserError> {
    let url = Url::parse(value).map_err(|_| BrowserError::UnsafeUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || value.chars().any(char::is_control)
    {
        return Err(BrowserError::UnsafeUrl);
    }
    Ok(url)
}

pub trait BrowserOpener {
    fn open_login_url(&self, url: &str) -> Result<(), BrowserError>;
}

#[derive(Clone, Debug, Default)]
pub struct MacOsBrowser;

impl BrowserOpener for MacOsBrowser {
    fn open_login_url(&self, value: &str) -> Result<(), BrowserError> {
        let url = validate_login_url(value)?;
        let status = Command::new("/usr/bin/open")
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| BrowserError::Open(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(BrowserError::Open(format!("open exited with {status}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{validate_login_url, AppPaths, BrowserError};

    #[test]
    fn application_data_never_uses_the_launch_directory() {
        let paths = AppPaths::from_home(Path::new("/Users/example"));
        assert_eq!(
            paths.support_dir,
            Path::new("/Users/example/Library/Application Support/vaire")
        );
        assert!(paths.preferences_file.starts_with(&paths.support_dir));
        assert_eq!(paths.openrouter_dir, paths.support_dir.join("openrouter"));
        assert_eq!(
            paths.openrouter_home_dir,
            paths.runtime_dir.join("openrouter-home")
        );
        assert_eq!(
            paths.openrouter_credential_file,
            paths.openrouter_home_dir.join("api-key")
        );
        assert_eq!(
            paths.claude_cli_home_dir,
            paths.runtime_dir.join("claude-home")
        );
        assert_eq!(
            paths.claude_conversation_dir,
            paths.runtime_dir.join("claude-conversation")
        );
        assert_eq!(
            paths.claude_probe_dir,
            paths.runtime_dir.join("claude-probes")
        );
        assert_eq!(
            paths.anthropic_credential_home_dir,
            paths.runtime_dir.join("anthropic-home")
        );
        assert_eq!(
            paths.anthropic_credential_file,
            paths.anthropic_credential_home_dir.join("api-key")
        );
        assert_eq!(paths.claude_store_dir, paths.support_dir.join("claude"));
        #[cfg(target_os = "macos")]
        assert_eq!(
            paths.historical_conversation_dir,
            Some(
                Path::new("/Users/example")
                    .join("Library/Application Support/AgentHarness/runtime/conversation")
            )
        );
    }

    #[test]
    fn accepts_only_direct_safe_https_login_urls() {
        assert!(validate_login_url("https://auth.openai.com/oauth?state=opaque").is_ok());
        assert_eq!(
            validate_login_url("http://auth.openai.com/oauth"),
            Err(BrowserError::UnsafeUrl)
        );
        assert_eq!(
            validate_login_url("https://user:pass@auth.openai.com/oauth"),
            Err(BrowserError::UnsafeUrl)
        );
        assert_eq!(
            validate_login_url("https://auth.openai.com/\n--args"),
            Err(BrowserError::UnsafeUrl)
        );
    }
}
