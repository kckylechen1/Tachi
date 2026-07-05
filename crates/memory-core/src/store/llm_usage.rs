//! LLM usage ledger writes on [`MemoryStore`].

use crate::{error::MemoryError, MemoryStore};

#[derive(Debug, Clone)]
pub struct LlmUsageEvent {
    pub timestamp: String,
    pub lane: String,
    pub model: String,
    pub provider_host: String,
    pub provider_logical_name: String,
    pub provider_key_id: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub max_tokens: i64,
    pub request_chars: i64,
    pub response_chars: i64,
    pub duration_ms: i64,
}

impl MemoryStore {
    /// Record one successful chat-lane call in the `llm_usage` ledger.
    pub fn record_llm_usage(&self, event: &LlmUsageEvent) -> Result<(), MemoryError> {
        self.conn.execute(
            "INSERT INTO llm_usage (
                timestamp, lane, model, provider_host, provider_logical_name,
                provider_key_id, prompt_tokens, completion_tokens, total_tokens,
                max_tokens, request_chars, response_chars, duration_ms, success,
                created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?1)",
            rusqlite::params![
                event.timestamp,
                event.lane,
                event.model,
                event.provider_host,
                event.provider_logical_name,
                event.provider_key_id,
                event.prompt_tokens,
                event.completion_tokens,
                event.total_tokens,
                event.max_tokens,
                event.request_chars,
                event.response_chars,
                event.duration_ms,
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_llm_usage_inserts_ledger_row() {
        let store = MemoryStore::open_in_memory().expect("open test store");
        let event = LlmUsageEvent {
            timestamp: "2026-07-05T00:00:00.000Z".to_string(),
            lane: "extract".to_string(),
            model: "test-model".to_string(),
            provider_host: "api.example.com".to_string(),
            provider_logical_name: "example".to_string(),
            provider_key_id: "key-1".to_string(),
            prompt_tokens: Some(120),
            completion_tokens: Some(30),
            total_tokens: Some(150),
            max_tokens: 300,
            request_chars: 512,
            response_chars: 128,
            duration_ms: 42,
        };

        store.record_llm_usage(&event).expect("record usage");

        let (lane, total_tokens, success, created_at): (String, i64, i64, String) = store
            .connection()
            .query_row(
                "SELECT lane, total_tokens, success, created_at FROM llm_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read usage row");
        assert_eq!(lane, "extract");
        assert_eq!(total_tokens, 150);
        assert_eq!(success, 1);
        assert_eq!(created_at, "2026-07-05T00:00:00.000Z");
    }
}
