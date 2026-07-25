use crate::app::Intent;

pub const HELP_TEXT: &str = "\
Commands:\n  /login                 Choose Codex, OpenRouter, or Claude authentication\n  /login browser         Start Codex browser callback sign-in directly\n  /login device          Start Codex device-code sign-in directly\n  /logout                Choose a provider to sign out\n  /model                 Browse searchable provider-labelled models\n  /reasoning [value]     List or select Codex reasoning / Claude effort\n  /new                   Start and switch to a new conversation\n  /resume                Browse saved Codex, OpenRouter, and Claude conversations\n  /thinking              Toggle the Reasoning panel\n  /help                  Show this help\n  /quit                  Exit Vairë\nProvider popup: arrows or j/k move; Enter selects. OpenRouter uses masked API-key entry; Claude opens the native Claude Code browser login for subscription authentication. c manages the OpenRouter catalog, r refreshes OpenRouter or Claude status, and d starts Codex device login. Signing Claude out also signs the system Claude Code CLI out.\nModel/catalog: type to search, Backspace edits, Space toggles OpenRouter catalog models, Enter commits, Esc discards or closes.\nConversation picker: arrows or j/k move, Enter resumes, d removes one inactive history, D removes all inactive histories, Esc closes. Every removal requires Enter confirmation; Claude removal forgets only Vairë's registration/display history.\nSwitching provider starts a new conversation; changing a Claude alias also starts blank. Use /resume for history. Claude /reasoning default uses the provider default.\nCodex and Claude tools run with unrestricted same-user access and no approval UI; this is not a sandbox.\nKeys: Enter sends, Alt-Enter inserts a newline, Escape interrupts or closes help, PageUp/PageDown scroll, Ctrl-C quits.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    Empty,
    UnknownCommand(String),
    UnexpectedArgument(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "enter a message or /help"),
            Self::UnknownCommand(command) => {
                write!(formatter, "unknown command {command}; use /help")
            }
            Self::UnexpectedArgument(command) if command == "/login" => {
                write!(formatter, "use /login, /login browser, or /login device")
            }
            Self::UnexpectedArgument(command) => {
                write!(formatter, "{command} does not accept an argument")
            }
        }
    }
}

pub fn parse(input: &str) -> Result<Intent, ParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ParseError::Empty);
    }
    if !input.starts_with('/') {
        return Ok(Intent::SendMessage(input.to_owned()));
    }

    let mut parts = input.splitn(2, char::is_whitespace);
    let command = parts.next().expect("non-empty input");
    let argument = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let no_argument = |intent| {
        if argument.is_some() {
            Err(ParseError::UnexpectedArgument(command.to_owned()))
        } else {
            Ok(intent)
        }
    };

    match command {
        "/login" => match argument {
            None => Ok(Intent::ShowLogin),
            Some("browser") => Ok(Intent::Login),
            Some("device") => Ok(Intent::LoginDevice),
            Some(_) => Err(ParseError::UnexpectedArgument(command.to_owned())),
        },
        "/logout" => no_argument(Intent::ShowLogout),
        "/new" => no_argument(Intent::NewThread),
        "/model" => no_argument(Intent::ShowModels),
        "/reasoning" => Ok(argument.map_or(Intent::ShowReasoning, |value| {
            Intent::SelectReasoning(value.to_owned())
        })),
        "/thinking" => no_argument(Intent::ToggleThinking),
        "/resume" => no_argument(Intent::Resume),
        "/help" => no_argument(Intent::Help),
        "/quit" => no_argument(Intent::Quit),
        other => Err(ParseError::UnknownCommand(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, ParseError, HELP_TEXT};
    use crate::app::Intent;

    #[test]
    fn parses_text_and_supported_commands() {
        assert_eq!(
            parse("  hello there  "),
            Ok(Intent::SendMessage("hello there".to_owned()))
        );
        assert_eq!(parse("/model"), Ok(Intent::ShowModels));
        assert_eq!(
            parse("/reasoning high"),
            Ok(Intent::SelectReasoning("high".to_owned()))
        );
        assert_eq!(parse("/resume"), Ok(Intent::Resume));
        assert_eq!(parse("/new"), Ok(Intent::NewThread));
        assert_eq!(parse("/thinking"), Ok(Intent::ToggleThinking));
        assert_eq!(parse("/reasoning"), Ok(Intent::ShowReasoning));
        assert_eq!(parse("/login"), Ok(Intent::ShowLogin));
        assert_eq!(parse("/login browser"), Ok(Intent::Login));
        assert_eq!(parse("/login device"), Ok(Intent::LoginDevice));
        assert_eq!(parse("/logout"), Ok(Intent::ShowLogout));
        assert_eq!(parse("/help"), Ok(Intent::Help));
        assert_eq!(parse("/quit"), Ok(Intent::Quit));
        assert_eq!(
            parse("\n first line\nsecond line \t"),
            Ok(Intent::SendMessage("first line\nsecond line".to_owned()))
        );
    }

    #[test]
    fn unknown_commands_and_bad_arguments_stay_local() {
        assert_eq!(
            parse("/branch now"),
            Err(ParseError::UnknownCommand("/branch".to_owned()))
        );
        assert_eq!(
            parse("/login api-key"),
            Err(ParseError::UnexpectedArgument("/login".to_owned()))
        );
        assert_eq!(
            parse("/model codex-current"),
            Err(ParseError::UnexpectedArgument("/model".to_owned()))
        );
        assert_eq!(
            parse("/thinking hidden"),
            Err(ParseError::UnexpectedArgument("/thinking".to_owned()))
        );
        assert_eq!(parse(" \n\t "), Err(ParseError::Empty));
        assert_eq!(
            parse("/quit\nthis must stay local"),
            Err(ParseError::UnexpectedArgument("/quit".to_owned()))
        );
        assert_eq!(
            parse("/branch\nthis must stay local"),
            Err(ParseError::UnknownCommand("/branch".to_owned()))
        );
        assert_eq!(
            parse("/login api-key").unwrap_err().to_string(),
            "use /login, /login browser, or /login device"
        );
        assert!(HELP_TEXT.contains("/login device"));
        assert!(HELP_TEXT.contains("Choose Codex, OpenRouter, or Claude authentication"));
        assert!(HELP_TEXT.contains("manages the OpenRouter catalog"));
        assert!(HELP_TEXT.contains("saved Codex, OpenRouter, and Claude conversations"));
        assert!(HELP_TEXT.contains("D removes all inactive histories"));
        assert!(HELP_TEXT.contains("changing a Claude alias also starts blank"));
        assert!(HELP_TEXT.contains("native Claude Code browser login"));
        assert!(HELP_TEXT.contains("system Claude Code CLI out"));
        assert!(HELP_TEXT.contains("unrestricted same-user access"));
        assert!(HELP_TEXT.contains("/quit"));
        assert!(HELP_TEXT.contains("/thinking              Toggle the Reasoning panel"));
        assert!(HELP_TEXT
            .contains("/reasoning [value]     List or select Codex reasoning / Claude effort"));
        assert!(HELP_TEXT.contains("Claude /reasoning default uses the provider default"));
        assert!(HELP_TEXT.contains("Escape interrupts"));
    }
}
