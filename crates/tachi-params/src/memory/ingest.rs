use super::{default_importance, default_scope, default_true};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_extraction_source() -> String {
    "extraction".to_string()
}

fn default_ingest_type() -> String {
    "source".to_string()
}

fn default_auto_chunk() -> bool {
    true
}

fn default_chunk_size_chars() -> usize {
    1200
}

fn default_chunk_overlap_chars() -> usize {
    120
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct ExtractFactsParams {
    /// Text to extract facts from
    pub text: String,

    /// Source identifier for the extraction
    #[serde(default = "default_extraction_source")]
    pub source: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,
}

/// A single message in a conversation turn.
///
/// Some MCP clients send compact session windows as raw strings even though the
/// schema advertises `{role, content}` objects. Accept both shapes at the
/// boundary so callers get deterministic tool behavior instead of serde
/// transport errors.
#[derive(Debug, Clone, serde::Serialize, JsonSchema)]
pub struct Message {
    /// Role of the message sender (e.g., "user", "assistant", "system")
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// Content of the message
    pub content: String,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum MessageInput {
            Object {
                #[serde(default = "default_message_role")]
                role: String,
                content: String,
            },
            Text(String),
        }

        match MessageInput::deserialize(deserializer)? {
            MessageInput::Object { role, content } => Ok(Message { role, content }),
            MessageInput::Text(content) => Ok(Message {
                role: default_message_role(),
                content,
            }),
        }
    }
}

fn default_message_role() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IngestEventParams {
    /// Conversation identifier
    #[serde(default)]
    pub conversation_id: String,

    /// Turn identifier
    #[serde(default)]
    pub turn_id: String,

    /// Optional event type label for structured events
    #[serde(default)]
    pub event_type: Option<String>,

    /// Optional structured event payload
    #[serde(default)]
    pub content: Option<serde_json::Value>,

    /// Messages in the conversation turn
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Optional path prefix override for structured event writes
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional write importance for structured event writes
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub importance: Option<f64>,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct IngestSourceParams {
    /// Raw source content to ingest
    pub content: String,

    /// Optional source URL or canonical reference
    #[serde(default)]
    pub source_url: Option<String>,

    /// Optional logical source identifier
    #[serde(default)]
    pub source: Option<String>,

    /// Optional path prefix used for chunk paths
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Whether to chunk long content before storage
    #[serde(default = "default_auto_chunk")]
    pub auto_chunk: bool,

    /// Whether to generate summaries for stored chunks
    #[serde(default = "default_true")]
    pub auto_summarize: bool,

    /// Whether to build graph edges against similar memories
    #[serde(default = "default_true")]
    pub auto_link: bool,

    /// Base importance for stored chunks
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Chunk size in characters
    #[serde(default = "default_chunk_size_chars")]
    pub chunk_size_chars: usize,

    /// Overlap between adjacent chunks in characters
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IngestParams {
    /// Ingest mode: "event" or "source"
    #[serde(default = "default_ingest_type")]
    pub ingest_type: String,

    /// Raw source content or structured event payload
    #[serde(default)]
    pub content: Option<serde_json::Value>,

    /// Optional source URL or canonical reference
    #[serde(default)]
    pub source_url: Option<String>,

    /// Optional logical source identifier
    #[serde(default)]
    pub source: Option<String>,

    /// Optional path prefix used for chunk paths
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Whether to chunk long content before storage
    #[serde(default = "default_auto_chunk")]
    pub auto_chunk: bool,

    /// Whether to generate summaries for stored chunks
    #[serde(default = "default_true")]
    pub auto_summarize: bool,

    /// Whether to build graph edges against similar memories
    #[serde(default = "default_true")]
    pub auto_link: bool,

    /// Base importance for stored chunks
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Chunk size in characters
    #[serde(default = "default_chunk_size_chars")]
    pub chunk_size_chars: usize,

    /// Overlap between adjacent chunks in characters
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// Conversation identifier for event ingestion
    #[serde(default)]
    pub conversation_id: Option<String>,

    /// Turn identifier for event ingestion
    #[serde(default)]
    pub turn_id: Option<String>,

    /// Event type label for event ingestion
    #[serde(default)]
    pub event_type: Option<String>,

    /// Messages in the conversation turn
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
