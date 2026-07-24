use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The complete provider set supported by this milestone.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    #[default]
    Codex,
    OpenRouter,
    Claude,
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Codex => "Codex",
            Self::OpenRouter => "OpenRouter",
            Self::Claude => "Claude Code",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModelKey {
    pub provider: ProviderId,
    pub id: String,
}

impl ModelKey {
    pub fn new(provider: ProviderId, id: impl Into<String>) -> Result<Self, ProviderIdentityError> {
        let id = id.into();
        if id.is_empty() || id.len() > 512 || id != id.trim() || id.chars().any(char::is_control) {
            return Err(ProviderIdentityError::InvalidModelId);
        }
        Ok(Self { provider, id })
    }

    pub fn codex(id: impl Into<String>) -> Result<Self, ProviderIdentityError> {
        Self::new(ProviderId::Codex, id)
    }

    pub fn openrouter(id: impl Into<String>) -> Result<Self, ProviderIdentityError> {
        Self::new(ProviderId::OpenRouter, id)
    }

    pub fn claude(id: impl Into<String>) -> Result<Self, ProviderIdentityError> {
        Self::new(ProviderId::Claude, id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeModelAlias {
    Default,
    Fable,
    Opus,
    Sonnet,
    Haiku,
}

impl ClaudeModelAlias {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fable => "fable",
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }
}

impl fmt::Display for ClaudeModelAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ClaudeModelAlias {
    type Err = ProviderIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "default" => Ok(Self::Default),
            "fable" => Ok(Self::Fable),
            "opus" => Ok(Self::Opus),
            "sonnet" => Ok(Self::Sonnet),
            "haiku" => Ok(Self::Haiku),
            _ => Err(ProviderIdentityError::InvalidClaudeModelAlias),
        }
    }
}

macro_rules! openrouter_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, Uuid::new_v4().simple()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            fn parse(value: &str) -> Result<Self, ProviderIdentityError> {
                let Some(hex) = value.strip_prefix($prefix) else {
                    return Err(ProviderIdentityError::InvalidOpenRouterId);
                };
                if hex.len() != 32
                    || hex
                        .bytes()
                        .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
                {
                    return Err(ProviderIdentityError::InvalidOpenRouterId);
                }
                let parsed =
                    Uuid::parse_str(hex).map_err(|_| ProviderIdentityError::InvalidOpenRouterId)?;
                if parsed.simple().to_string() != hex {
                    return Err(ProviderIdentityError::InvalidOpenRouterId);
                }
                Ok(Self(value.to_owned()))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ProviderIdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ProviderIdentityError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

openrouter_id!(OpenRouterConversationId, "or_");
openrouter_id!(OpenRouterTurnId, "ort_");

macro_rules! claude_uuid_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4().hyphenated().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            fn parse(value: &str) -> Result<Self, ProviderIdentityError> {
                let parsed =
                    Uuid::parse_str(value).map_err(|_| ProviderIdentityError::InvalidClaudeId)?;
                if parsed.hyphenated().to_string() != value {
                    return Err(ProviderIdentityError::InvalidClaudeId);
                }
                Ok(Self(value.to_owned()))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ProviderIdentityError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ProviderIdentityError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

claude_uuid_id!(ClaudeSessionId);
claude_uuid_id!(ClaudeTurnId);

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ConversationRef {
    Codex {
        thread_id: String,
    },
    OpenRouter {
        conversation_id: OpenRouterConversationId,
    },
    Claude {
        session_id: ClaudeSessionId,
    },
}

impl ConversationRef {
    pub fn provider(&self) -> ProviderId {
        match self {
            Self::Codex { .. } => ProviderId::Codex,
            Self::OpenRouter { .. } => ProviderId::OpenRouter,
            Self::Claude { .. } => ProviderId::Claude,
        }
    }

    pub fn id_str(&self) -> &str {
        match self {
            Self::Codex { thread_id } => thread_id,
            Self::OpenRouter { conversation_id } => conversation_id.as_str(),
            Self::Claude { session_id } => session_id.as_str(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum TurnRef {
    Codex {
        thread_id: String,
        turn_id: String,
    },
    OpenRouter {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
    },
    Claude {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
    },
}

impl TurnRef {
    pub fn provider(&self) -> ProviderId {
        match self {
            Self::Codex { .. } => ProviderId::Codex,
            Self::OpenRouter { .. } => ProviderId::OpenRouter,
            Self::Claude { .. } => ProviderId::Claude,
        }
    }

    pub fn conversation(&self) -> ConversationRef {
        match self {
            Self::Codex { thread_id, .. } => ConversationRef::Codex {
                thread_id: thread_id.clone(),
            },
            Self::OpenRouter {
                conversation_id, ..
            } => ConversationRef::OpenRouter {
                conversation_id: conversation_id.clone(),
            },
            Self::Claude { session_id, .. } => ConversationRef::Claude {
                session_id: session_id.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningCapability {
    CodexEfforts {
        default_effort: String,
        supported_efforts: Vec<String>,
    },
    Unsupported,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ProviderIdentityError {
    #[error("invalid provider model ID")]
    InvalidModelId,
    #[error("invalid OpenRouter identity")]
    InvalidOpenRouterId,
    #[error("invalid Claude identity")]
    InvalidClaudeId,
    #[error("invalid Claude model alias")]
    InvalidClaudeModelAlias,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_ids_are_canonical_and_serde_validated() {
        let conversation = OpenRouterConversationId::new();
        let turn = OpenRouterTurnId::new();
        assert_eq!(conversation.as_str().len(), 35);
        assert!(conversation.as_str().starts_with("or_"));
        assert_eq!(turn.as_str().len(), 36);
        assert!(turn.as_str().starts_with("ort_"));

        let encoded = serde_json::to_string(&conversation).unwrap();
        assert_eq!(
            serde_json::from_str::<OpenRouterConversationId>(&encoded).unwrap(),
            conversation
        );
        for invalid in [
            "or_1234",
            "or_0000000000000000000000000000000G",
            "OR_00000000000000000000000000000000",
            "ort_00000000000000000000000000000000",
        ] {
            assert!(invalid.parse::<OpenRouterConversationId>().is_err());
        }
    }

    #[test]
    fn provider_tags_prevent_cross_provider_identity_collisions() {
        let conversation_id: OpenRouterConversationId =
            "or_00000000000000000000000000000000".parse().unwrap();
        let codex = ConversationRef::Codex {
            thread_id: conversation_id.to_string(),
        };
        let openrouter = ConversationRef::OpenRouter { conversation_id };
        assert_ne!(codex, openrouter);
        assert_eq!(codex.provider(), ProviderId::Codex);
        assert_eq!(openrouter.provider(), ProviderId::OpenRouter);
    }

    #[test]
    fn model_keys_are_bounded_but_allow_openrouter_slashes() {
        assert_eq!(
            ModelKey::openrouter("anthropic/claude").unwrap().provider,
            ProviderId::OpenRouter
        );
        for invalid in ["", " model", "model\n"] {
            assert!(ModelKey::codex(invalid).is_err());
        }
    }

    #[test]
    fn claude_aliases_have_stable_lowercase_strings() {
        for (alias, expected) in [
            (ClaudeModelAlias::Default, "default"),
            (ClaudeModelAlias::Fable, "fable"),
            (ClaudeModelAlias::Opus, "opus"),
            (ClaudeModelAlias::Sonnet, "sonnet"),
            (ClaudeModelAlias::Haiku, "haiku"),
        ] {
            assert_eq!(alias.as_str(), expected);
            assert_eq!(alias.to_string(), expected);
            assert_eq!(expected.parse::<ClaudeModelAlias>().unwrap(), alias);
            let encoded = serde_json::to_string(&alias).unwrap();
            assert_eq!(encoded, format!("\"{expected}\""));
            assert_eq!(
                serde_json::from_str::<ClaudeModelAlias>(&encoded).unwrap(),
                alias
            );
        }
        assert!("Sonnet".parse::<ClaudeModelAlias>().is_err());
        assert_eq!(ProviderId::Claude.to_string(), "Claude Code");
        assert_eq!(
            serde_json::to_string(&ProviderId::Claude).unwrap(),
            "\"claude\""
        );
    }

    #[test]
    fn claude_ids_require_canonical_hyphenated_uuids_and_remain_provider_tagged() {
        let session: ClaudeSessionId = "00000000-0000-4000-8000-000000000000".parse().unwrap();
        let turn: ClaudeTurnId = "11111111-1111-4111-8111-111111111111".parse().unwrap();
        let conversation = ConversationRef::Claude {
            session_id: session.clone(),
        };
        let turn_ref = TurnRef::Claude {
            session_id: session,
            turn_id: turn,
        };
        assert_eq!(conversation.provider(), ProviderId::Claude);
        assert_eq!(
            conversation.id_str(),
            "00000000-0000-4000-8000-000000000000"
        );
        assert_eq!(turn_ref.conversation(), conversation);
        for invalid in [
            "00000000000040008000000000000000",
            "00000000-0000-4000-8000-00000000000A",
            "not-a-uuid",
        ] {
            assert!(invalid.parse::<ClaudeSessionId>().is_err());
        }
    }
}
