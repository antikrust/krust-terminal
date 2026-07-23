use std::fmt::Display;

use derp_multi_agent_api as api;

/// A citation listed in an AI response.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum AIAgentCitation {
    DerpDriveObject {
        uid: String,
    },
    DerpDocumentation {
        path: String,
    },
    WebPage {
        url: String,
    },
    /// A memory from an attached memory store. `content` is the raw memory
    /// text shown as a preview in the chip.
    AgentMemory {
        memory_store_id: String,
        memory_id: String,
        content: String,
    },
}

impl Display for AIAgentCitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AIAgentCitation::DerpDriveObject { uid } => {
                write!(f, "Derp Drive Object: {uid}")
            }
            AIAgentCitation::DerpDocumentation { path } => {
                write!(f, "Derp Documentation: {path}")
            }
            AIAgentCitation::WebPage { url } => {
                write!(f, "Web Page: {url}")
            }
            AIAgentCitation::AgentMemory {
                memory_store_id,
                memory_id,
                ..
            } => {
                write!(f, "Agent Memory: {memory_store_id}/{memory_id}")
            }
        }
    }
}

/// Error type for Citation conversion errors
#[derive(Debug, thiserror::Error)]
#[error("Unknown citation type")]
pub struct UnknownCitationTypeError;

impl TryFrom<api::Citation> for AIAgentCitation {
    type Error = UnknownCitationTypeError;

    fn try_from(citation: api::Citation) -> Result<Self, Self::Error> {
        let doc_type = api::DocumentType::try_from(citation.document_type)
            .unwrap_or(api::DocumentType::Unknown);

        match doc_type {
            api::DocumentType::DerpDriveWorkflow
            | api::DocumentType::DerpDriveNotebook
            | api::DocumentType::DerpDriveEnvVar
            | api::DocumentType::Rule => Ok(AIAgentCitation::DerpDriveObject {
                uid: citation.document_id,
            }),
            api::DocumentType::DerpDocumentation => Ok(AIAgentCitation::DerpDocumentation {
                path: citation.document_id,
            }),
            api::DocumentType::WebPage => Ok(AIAgentCitation::WebPage {
                url: citation.document_id,
            }),
            api::DocumentType::Unknown => Err(UnknownCitationTypeError),
        }
    }
}
