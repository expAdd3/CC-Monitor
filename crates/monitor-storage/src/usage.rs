/// Exact, non-negative usage totals. Every addition saturates only when the
/// signed SQLite/UI representation would overflow; otherwise it remains an
/// exact integer operation (including values above 2^53).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost_pico_usd: i64,
    pub cost_known: bool,
    pub unpriced_tokens: i64,
}

impl UsageTotals {
    pub fn from_values(
        input_tokens: i64,
        output_tokens: i64,
        cache_write_tokens: i64,
        cache_read_tokens: i64,
        cost_pico_usd: i64,
        cost_known: bool,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_write_tokens,
            cache_read_tokens,
            cost_pico_usd,
            cost_known,
            unpriced_tokens: 0,
        }
    }

    pub fn add(&mut self, value: Self) {
        self.input_tokens = self.input_tokens.saturating_add(value.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(value.output_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(value.cache_write_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(value.cache_read_tokens);
        self.cost_pico_usd = self.cost_pico_usd.saturating_add(value.cost_pico_usd);
        self.cost_known &= value.cost_known;
        self.unpriced_tokens = self.unpriced_tokens.saturating_add(value.unpriced_tokens);
    }

    pub fn tokens(self) -> i64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}
