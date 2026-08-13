use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub request_id: String,
    pub model: String,
    pub prompt: String,
    pub stream: bool,
    pub include_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: u32,
}

impl Usage {
    #[must_use]
    pub const fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GenerationEvent {
    Prepared {
        prompt_tokens: u32,
    },
    Content {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    Completed {
        finish_reason: String,
        usage: Usage,
    },
    Failed {
        message: String,
        code: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{GenerationEvent, Usage};

    #[test]
    fn events_round_trip_as_tagged_json() {
        let event = GenerationEvent::Completed {
            finish_reason: "stop".into(),
            usage: Usage {
                prompt_tokens: 3,
                completion_tokens: 1,
                cached_tokens: 0,
            },
        };
        let json = serde_json::to_string(&event).expect("serialize event");
        assert!(json.contains(r#""type":"completed""#));
        assert_eq!(
            serde_json::from_str::<GenerationEvent>(&json).expect("deserialize event"),
            event
        );
    }
}
