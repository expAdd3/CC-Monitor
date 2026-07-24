use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PICO_PER_USD: i128 = 1_000_000_000_000;
pub const TOKENS_PER_MILLION: i128 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl TokenUsage {
    pub fn total(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cost {
    pub pico_usd: i64,
    pub known: bool,
}

#[derive(Clone, Debug)]
pub struct PriceCatalog {
    prices: BTreeMap<String, Price>,
}

impl Default for PriceCatalog {
    fn default() -> Self {
        let raw: BTreeMap<String, Vec<serde_json::Number>> =
            serde_json::from_str(include_str!("../../../prices.builtin.json"))
                .expect("bundled price catalog must be valid");
        let prices = raw
            .into_iter()
            .map(|(model, p)| {
                assert_eq!(p.len(), 4, "price entries contain four rates");
                let exact = |index: usize| {
                    decimal_usd_to_pico(&p[index].to_string())
                        .expect("bundled price is an exact bounded decimal")
                };
                (
                    normalize_model(&model, false),
                    Price {
                        input: exact(0),
                        cache_write: exact(1),
                        cache_read: exact(2),
                        output: exact(3),
                    },
                )
            })
            .collect();
        Self { prices }
    }
}

impl PriceCatalog {
    pub fn with_overrides(mut self, overrides: impl IntoIterator<Item = (String, Price)>) -> Self {
        for (model, price) in overrides {
            self.prices.insert(normalize_model(&model, false), price);
        }
        self
    }

    pub fn price_for(&self, model: &str) -> Option<Price> {
        for candidate in [normalize_model(model, false), normalize_model(model, true)] {
            if let Some(price) = self.prices.get(&candidate) {
                return Some(*price);
            }
            let base = strip_date(&candidate);
            let matches: Vec<_> = self
                .prices
                .iter()
                .filter(|(key, _)| strip_date(key) == base)
                .map(|(_, price)| *price)
                .collect();
            if matches.len() == 1 {
                return matches.first().copied();
            }
            if matches.len() > 1 {
                return None;
            }
        }
        None
    }

    pub fn cost(&self, usage: TokenUsage, model: &str) -> Cost {
        let Some(price) = self.price_for(model) else {
            return Cost {
                pico_usd: 0,
                known: false,
            };
        };
        let component =
            |tokens: u64, rate: i64| i128::from(tokens).saturating_mul(i128::from(rate));
        let numerator = component(usage.input, price.input)
            .saturating_add(component(usage.output, price.output))
            .saturating_add(component(usage.cache_write, price.cache_write))
            .saturating_add(component(usage.cache_read, price.cache_read));
        Cost {
            pico_usd: i64::try_from(numerator / TOKENS_PER_MILLION).unwrap_or(i64::MAX),
            known: true,
        }
    }
}

pub fn extract_usage(value: &serde_json::Value) -> TokenUsage {
    let n = |name: &str| value.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
    let mut input = value
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| n("prompt_tokens"));
    let output = value
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| n("completion_tokens"));
    let cache_write = n("cache_creation_input_tokens");
    let mut cache_read = n("cache_read_input_tokens");
    if cache_read == 0 {
        cache_read = value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(input);
        input -= cache_read;
    }
    TokenUsage {
        input,
        output,
        cache_write,
        cache_read,
    }
}

/// Parses non-negative decimal USD without binary floating point. More than 12
/// fractional digits are rounded half-up to pico-USD; at most 18 are accepted.
pub fn decimal_usd_to_pico(value: &str) -> Result<i64, &'static str> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.starts_with('-') || fraction.len() > 18 || whole.is_empty() {
        return Err("invalid decimal");
    }
    let whole: i128 = whole.parse().map_err(|_| "invalid decimal")?;
    let kept = &fraction[..fraction.len().min(12)];
    let mut fractional: i128 = if kept.is_empty() {
        0
    } else {
        kept.parse().map_err(|_| "invalid decimal")?
    };
    fractional = fractional
        .checked_mul(10_i128.pow((12 - kept.len()) as u32))
        .ok_or("price overflow")?;
    if fraction
        .as_bytes()
        .get(12)
        .is_some_and(|digit| *digit >= b'5')
    {
        fractional = fractional.checked_add(1).ok_or("price overflow")?;
    }
    let pico = whole
        .checked_mul(PICO_PER_USD)
        .and_then(|base| base.checked_add(fractional))
        .ok_or("price overflow")?;
    i64::try_from(pico).map_err(|_| "price overflow")
}

fn normalize_model(model: &str, remove_provider: bool) -> String {
    let mut value = model.trim().to_ascii_lowercase();
    if remove_provider {
        value = value.rsplit('/').next().unwrap_or_default().to_owned();
    }
    value = value.split(':').next().unwrap_or_default().to_owned();
    if value.ends_with("[1m]") {
        value.truncate(value.len() - 4);
    }
    let mut out = String::with_capacity(value.len());
    let mut dash = false;
    for ch in value.chars() {
        let ch = if matches!(ch, '@' | '_' | '.') {
            '-'
        } else {
            ch
        };
        if ch == '-' {
            if !dash {
                out.push(ch);
            }
            dash = true;
        } else {
            out.push(ch);
            dash = false;
        }
    }
    out.trim_matches('-').to_owned()
}

fn strip_date(model: &str) -> &str {
    if model.len() >= 9 {
        let tail = &model[model.len() - 9..];
        if tail.starts_with('-') && tail[1..].bytes().all(|b| b.is_ascii_digit()) {
            return &model[..model.len() - 9];
        }
    }
    if model.len() >= 11 {
        let tail = &model[model.len() - 11..];
        if tail.as_bytes()[0] == b'-'
            && tail.bytes().enumerate().all(|(i, b)| {
                if i == 0 || i == 5 || i == 8 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            return &model[..model.len() - 11];
        }
    }
    model
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_and_prices_exactly_in_pico_usd() {
        let catalog = PriceCatalog::default();
        let usage = TokenUsage {
            input: 100,
            output: 50,
            cache_write: 20,
            cache_read: 30,
        };
        assert_eq!(
            catalog
                .cost(usage, "anthropic/claude-sonnet-4-5:beta")
                .pico_usd,
            1_134_000_000
        );
    }

    #[test]
    fn extracts_openai_cached_tokens() {
        assert_eq!(
            extract_usage(&json!({"prompt_tokens":200,"completion_tokens":10,
                "prompt_tokens_details":{"cached_tokens":40}})),
            TokenUsage {
                input: 160,
                output: 10,
                cache_write: 0,
                cache_read: 40
            }
        );
    }

    #[test]
    fn decimal_prices_are_exact_rounded_and_checked() {
        assert_eq!(decimal_usd_to_pico("0.003625").unwrap(), 3_625_000_000);
        assert_eq!(
            decimal_usd_to_pico("1.0000000000005").unwrap(),
            1_000_000_000_001
        );
        assert!(decimal_usd_to_pico("999999999999999999999").is_err());
        assert!(decimal_usd_to_pico("1.1234567890123456789").is_err());
    }
}
