use std::ops::AddAssign;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens = sum(self.input_tokens, rhs.input_tokens);
        self.output_tokens = sum(self.output_tokens, rhs.output_tokens);
        self.total_tokens = sum(self.total_tokens, rhs.total_tokens);
        self.cache_read_tokens = sum(self.cache_read_tokens, rhs.cache_read_tokens);
        self.cache_write_tokens = sum(self.cache_write_tokens, rhs.cache_write_tokens);
        self.reasoning_tokens = sum(self.reasoning_tokens, rhs.reasoning_tokens);
    }
}

fn sum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left?.checked_add(right?)
}

#[cfg(test)]
mod tests {
    use super::Usage;

    #[test]
    fn turn_total_only_reports_fields_known_for_every_cycle() {
        let mut total = Usage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: Some(1),
        };
        total += Usage {
            input_tokens: Some(20),
            output_tokens: Some(3),
            total_tokens: Some(23),
            cache_read_tokens: Some(8),
            cache_write_tokens: None,
            reasoning_tokens: None,
        };
        assert_eq!(total.input_tokens, Some(30));
        assert_eq!(total.output_tokens, Some(5));
        assert_eq!(total.total_tokens, Some(35));
        assert_eq!(total.cache_read_tokens, None);
        assert_eq!(total.cache_write_tokens, None);
        assert_eq!(total.reasoning_tokens, None);
    }
}
