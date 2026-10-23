use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Token usage summary — what the API billed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

impl Usage {
    /// Every billed token: uncached input, cache reads and writes, output.
    #[must_use]
    pub fn total(self) -> u32 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_input_tokens.unwrap_or(0))
            .saturating_add(self.cache_creation_input_tokens.unwrap_or(0))
    }

    pub fn merge(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_creation_input_tokens = add_opt(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.cache_read_input_tokens =
            add_opt(self.cache_read_input_tokens, other.cache_read_input_tokens);
    }
}

fn add_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sums_tokens() {
        let mut a = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: Some(30),
            ..Default::default()
        };
        a.merge(Usage {
            input_tokens: 20,
            output_tokens: 10,
            cache_read_input_tokens: Some(5),
            ..Default::default()
        });
        assert_eq!(a.input_tokens, 120);
        assert_eq!(a.output_tokens, 60);
        assert_eq!(a.cache_read_input_tokens, Some(35));
        // Cache reads are billed tokens too; total() used to drop them
        // (docs/token-efficiency.md A8), undercounting a cached session.
        assert_eq!(a.total(), 180 + 35);
    }
}
