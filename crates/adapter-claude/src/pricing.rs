use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const PICO_PER_USD: i128 = 1_000_000_000_000;
const TOKENS_PER_MILLION: i128 = 1_000_000;
/// Transcript JSON is untrusted. Bound each component so one record's four
/// token classes always fit in the signed SQLite representation and in a
/// saturated per-record total.
pub const MAX_TOKEN_COMPONENT: u64 = i64::MAX as u64 / 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Cost {
    pub(crate) pico_usd: i64,
    pub(crate) known: bool,
}

#[derive(Clone, Debug)]
pub struct PriceCatalog {
    prices: BTreeMap<String, Price>,
    tombstones: BTreeSet<String>,
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
        Self {
            prices,
            tombstones: BTreeSet::new(),
        }
    }
}

impl PriceCatalog {
    /// Returns the shipped defaults keyed by canonical model ID. The desktop
    /// presents one flat editable list; internally these defaults also decide
    /// when deletion needs a tombstone.
    pub fn builtin_prices() -> BTreeMap<String, Price> {
        Self::default().prices
    }

    pub fn canonical_model_id(model: &str) -> String {
        normalize_model(model, false)
    }

    pub fn is_builtin(model: &str) -> bool {
        Self::builtin_prices().contains_key(&Self::canonical_model_id(model))
    }

    /// Applies persisted changes to the bundled catalog. `None` deliberately
    /// removes a bundled model without mutating the shipped price file.
    pub fn with_changes(changes: impl IntoIterator<Item = (String, Option<Price>)>) -> Self {
        let mut catalog = Self::default();
        for (model, price) in changes {
            let model = Self::canonical_model_id(&model);
            match price {
                Some(price) => {
                    catalog.tombstones.remove(&model);
                    catalog.prices.insert(model, price);
                }
                None => {
                    catalog.prices.remove(&model);
                    catalog.tombstones.insert(model);
                }
            }
        }
        catalog
    }

    /// Returns whether removing this exact visible row must leave a tombstone
    /// to prevent the resolver from falling through to a shipped alias/date.
    pub fn deletion_requires_tombstone(&self, model: &str) -> bool {
        let model = Self::canonical_model_id(model);
        if Self::is_builtin(&model) {
            return true;
        }
        let mut without = self.clone();
        without.prices.remove(&model);
        without.tombstones.remove(&model);
        without.price(&model).is_some()
    }

    fn price_for(&self, model: &str) -> Option<Price> {
        for candidate in [normalize_model(model, false), normalize_model(model, true)] {
            if self.tombstones.contains(&candidate) {
                return None;
            }
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
            let tombstones = self
                .tombstones
                .iter()
                .filter(|key| strip_date(key) == base)
                .count();
            if matches.len() == 1 && tombstones == 0 {
                return matches.first().copied();
            }
            if matches.len() + tombstones > 1 {
                return None;
            }
        }
        None
    }

    pub fn price(&self, model: &str) -> Option<Price> {
        self.price_for(model)
    }

    pub(crate) fn cost(&self, usage: TokenUsage, model: &str) -> Cost {
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

pub(crate) fn extract_usage(value: &serde_json::Value) -> TokenUsage {
    let n = |name: &str| {
        value
            .get(name)
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(MAX_TOKEN_COMPONENT)
    };
    let mut input = value
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .map(|value| value.min(MAX_TOKEN_COMPONENT))
        .unwrap_or_else(|| n("prompt_tokens"));
    let output = value
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .map(|value| value.min(MAX_TOKEN_COMPONENT))
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
    let (whole, fraction, has_decimal_point) = value
        .split_once('.')
        .map_or((value, "", false), |(whole, fraction)| {
            (whole, fraction, true)
        });
    if whole.is_empty()
        || !whole.bytes().all(|digit| digit.is_ascii_digit())
        || fraction.len() > 18
        || has_decimal_point && fraction.is_empty()
        || !fraction.bytes().all(|digit| digit.is_ascii_digit())
    {
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
        let split = model.len() - 9;
        let tail = &model.as_bytes()[split..];
        if tail[0] == b'-' && tail[1..].iter().all(u8::is_ascii_digit) {
            if let Some(base) = model.get(..split) {
                return base;
            }
        }
    }
    if model.len() >= 11 {
        let split = model.len() - 11;
        let tail = &model.as_bytes()[split..];
        if tail[0] == b'-'
            && tail.iter().copied().enumerate().all(|(i, b)| {
                if i == 0 || i == 5 || i == 8 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            if let Some(base) = model.get(..split) {
                return base;
            }
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
    fn canonical_ids_drive_listing_overrides_and_deletions() {
        let custom = Price {
            input: 1,
            output: 2,
            cache_write: 3,
            cache_read: 4,
        };
        assert_eq!(
            PriceCatalog::canonical_model_id(" GPT_5.6_SOL:beta "),
            "gpt-5-6-sol"
        );
        let catalog = PriceCatalog::with_changes([
            ("gpt_5.6_sol".to_owned(), Some(custom)),
            ("claude-fable-5".to_owned(), None),
            ("new_model".to_owned(), Some(custom)),
        ]);
        assert_eq!(catalog.price("gpt-5-6-sol"), Some(custom));
        assert_eq!(catalog.price("new-model"), Some(custom));
        assert!(catalog.price("claude-fable-5").is_none());
    }

    #[test]
    fn bundled_prices_expose_canonical_builtin_identity() {
        let prices = PriceCatalog::builtin_prices();
        assert!(prices.contains_key("gpt-5-6-sol"));
        assert!(PriceCatalog::is_builtin(" GPT_5.6_SOL:beta "));
        assert!(!PriceCatalog::is_builtin("my-custom-model"));
    }

    #[test]
    fn unicode_model_ids_do_not_panic_during_date_fallback() {
        let catalog = PriceCatalog::default();
        assert_eq!(catalog.price("alpha-ααααα"), None);
        assert_eq!(catalog.price("ααααα"), None);
    }

    #[test]
    fn changed_catalog_prices_and_unprices_usage() {
        let usage = TokenUsage {
            input: 1_000_000,
            output: 0,
            cache_write: 0,
            cache_read: 0,
        };
        let catalog = PriceCatalog::with_changes([
            (
                "custom_model".to_owned(),
                Some(Price {
                    input: 7,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                }),
            ),
            ("claude-fable-5".to_owned(), None),
        ]);
        assert_eq!(catalog.cost(usage, "custom-model").pico_usd, 7);
        assert!(!catalog.cost(usage, "claude-fable-5").known);
    }

    #[test]
    fn tombstones_block_provider_and_date_fallback_without_hiding_other_identities() {
        let custom = Price {
            input: 7,
            output: 0,
            cache_write: 0,
            cache_read: 0,
        };
        let provider = "anthropic/claude-fable-5";
        let provider_catalog = PriceCatalog::with_changes([(provider.into(), Some(custom))]);
        assert!(provider_catalog.deletion_requires_tombstone(provider));
        let provider_deleted = PriceCatalog::with_changes([(provider.into(), None)]);
        assert!(provider_deleted.price(provider).is_none());
        assert!(provider_deleted.price("claude-fable-5").is_some());

        let dated = "claude-sonnet-4-5-20250929";
        let date_deleted = PriceCatalog::with_changes([(dated.into(), None)]);
        assert!(date_deleted.price(dated).is_none());
        assert!(date_deleted.price("claude-sonnet-4-5").is_none());
        assert!(date_deleted.price("claude-haiku-4-5-20251001").is_some());
    }

    #[test]
    fn readding_an_exact_identity_clears_its_tombstone() {
        let custom = Price {
            input: 9,
            output: 10,
            cache_write: 11,
            cache_read: 12,
        };
        let provider = "anthropic/claude-fable-5";
        let catalog =
            PriceCatalog::with_changes([(provider.into(), None), (provider.into(), Some(custom))]);
        assert_eq!(catalog.price(provider), Some(custom));
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
    fn bounds_untrusted_token_components_before_storage_projection() {
        let usage = extract_usage(&json!({
            "input_tokens": u64::MAX,
            "output_tokens": u64::MAX,
            "cache_creation_input_tokens": u64::MAX,
            "cache_read_input_tokens": u64::MAX,
        }));
        assert_eq!(
            usage,
            TokenUsage {
                input: MAX_TOKEN_COMPONENT,
                output: MAX_TOKEN_COMPONENT,
                cache_write: MAX_TOKEN_COMPONENT,
                cache_read: MAX_TOKEN_COMPONENT,
            }
        );
        assert_eq!(
            usage
                .input
                .saturating_add(usage.output)
                .saturating_add(usage.cache_write)
                .saturating_add(usage.cache_read),
            MAX_TOKEN_COMPONENT * 4
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

    #[test]
    fn frontend_and_rust_share_price_validation_examples() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ValidRate {
            value: String,
            pico_usd: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CanonicalModel {
            value: String,
            canonical: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Contract {
            valid_rates: Vec<ValidRate>,
            invalid_rates: Vec<String>,
            out_of_range_rates: Vec<String>,
            canonical_models: Vec<CanonicalModel>,
        }

        let contract: Contract = serde_json::from_str(include_str!(
            "../../../tests/fixtures/model_price_validation.json"
        ))
        .unwrap();
        for case in contract.valid_rates {
            assert_eq!(
                decimal_usd_to_pico(&case.value),
                Ok(case.pico_usd.parse().unwrap()),
                "valid rate {:?}",
                case.value
            );
        }
        for value in contract.invalid_rates {
            assert_eq!(
                decimal_usd_to_pico(&value),
                Err("invalid decimal"),
                "invalid rate {value:?}"
            );
        }
        for value in contract.out_of_range_rates {
            assert_eq!(
                decimal_usd_to_pico(&value),
                Err("price overflow"),
                "out-of-range rate {value:?}"
            );
        }
        for case in contract.canonical_models {
            assert_eq!(
                PriceCatalog::canonical_model_id(&case.value),
                case.canonical
            );
        }
    }
}
