use crate::app::Intent;

pub const HELP_TEXT: &str = "\
Commands:\n  /login                 Sign in with ChatGPT in your browser\n  /login device          Use device-code sign-in if browser callback login fails\n  /login browser         Explicitly use browser callback sign-in\n  /logout                Sign out, or cancel a pending sign-in\n  /model [id]            List or select an available model\n  /reasoning [value]     List or select a reasoning level\n  /new                   Start and switch to a new thread\n  /resume                Browse and resume saved threads\n  /help                  Show this help\n  /quit                  Exit AgentHarness\nThread picker: arrows or j/k move, Enter resumes, d permanently deletes one inactive thread, D permanently clears all inactive threads, Esc closes. Deletion always requires Enter confirmation.\nKeys: Enter sends, Alt-Enter inserts a newline, Escape interrupts or closes help, PageUp/PageDown scroll, Ctrl-C quits.";

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
            None | Some("browser") => Ok(Intent::Login),
            Some("device") => Ok(Intent::LoginDevice),
            Some(_) => Err(ParseError::UnexpectedArgument(command.to_owned())),
        },
        "/logout" => no_argument(Intent::Logout),
        "/new" => no_argument(Intent::NewThread),
        "/model" => {
            Ok(argument.map_or(Intent::ShowModels, |id| Intent::SelectModel(id.to_owned())))
        }
        "/reasoning" => Ok(argument.map_or(Intent::ShowReasoning, |value| {
            Intent::SelectReasoning(value.to_owned())
        })),
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
            parse("/model codex-current"),
            Ok(Intent::SelectModel("codex-current".to_owned()))
        );
        assert_eq!(
            parse("/reasoning high"),
            Ok(Intent::SelectReasoning("high".to_owned()))
        );
        assert_eq!(parse("/resume"), Ok(Intent::Resume));
        assert_eq!(parse("/new"), Ok(Intent::NewThread));
        assert_eq!(parse("/login"), Ok(Intent::Login));
        assert_eq!(parse("/login browser"), Ok(Intent::Login));
        assert_eq!(parse("/login device"), Ok(Intent::LoginDevice));
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
            parse("/login api-key").unwrap_err().to_string(),
            "use /login, /login browser, or /login device"
        );
        assert!(HELP_TEXT.contains("/login device"));
        assert!(HELP_TEXT.contains("D permanently clears all inactive"));
        assert!(HELP_TEXT.contains("/quit"));
        assert!(HELP_TEXT.contains("Escape interrupts"));
    }
}
