use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The complete provider set supported by this milestone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    #[default]
    Codex,
    OpenRouter,
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Codex => "Codex",
            Self::OpenRouter => "OpenRouter",
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ConversationRef {
    Codex {
        thread_id: String,
    },
    OpenRouter {
        conversation_id: OpenRouterConversationId,
    },
}

impl ConversationRef {
    pub fn provider(&self) -> ProviderId {
        match self {
            Self::Codex { .. } => ProviderId::Codex,
            Self::OpenRouter { .. } => ProviderId::OpenRouter,
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
}

impl TurnRef {
    pub fn provider(&self) -> ProviderId {
        match self {
            Self::Codex { .. } => ProviderId::Codex,
            Self::OpenRouter { .. } => ProviderId::OpenRouter,
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
}
