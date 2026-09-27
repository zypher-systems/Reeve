//! Token → USD pricing. Unknown rates yield `None` (`$?.??`), never a fake `$0.00`.
//!
//! Started as a copy of Ryter's `spend.rs`. Differences: cache writes are
//! counted and priced, and OpenRouter's own cache read/write prices are used
//! instead of assuming cached input costs the same as fresh input.

use std::collections::HashMap;

use crate::config::{Config, PriceOverride};
use crate::llm::ModelInfo;

/// Token counts from a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// All prompt / input tokens, including cached reads and cache writes.
    pub input_tokens: u64,
    /// Completion / output tokens.
    pub output_tokens: u64,
    /// Input tokens read from the provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: u64,
    /// Input tokens written to the provider's prompt cache (Anthropic models
    /// bill these above the fresh-input rate).
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl Usage {
    /// Fold a later usage report into this one. Counts are cumulative, so
    /// the larger of each wins.
    #[must_use]
    pub fn merge(self, later: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.max(later.input_tokens),
            output_tokens: self.output_tokens.max(later.output_tokens),
            cached_tokens: self.cached_tokens.max(later.cached_tokens),
            cache_write_tokens: self.cache_write_tokens.max(later.cache_write_tokens),
        }
    }

    /// Input plus output.
    pub fn total(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Share of input served from cache, 0.0–1.0.
    pub fn cache_hit_ratio(self) -> f64 {
        if self.input_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f64 / self.input_tokens as f64
        }
    }
}

/// Two separate calls, summed.
impl std::ops::Add for Usage {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            cache_write_tokens: self.cache_write_tokens + other.cache_write_tokens,
        }
    }
}

/// USD per million tokens for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Fresh input.
    pub input_per_million: f64,
    /// Output.
    pub output_per_million: f64,
    /// Cache reads. `None` bills them as fresh input.
    pub cache_read_per_million: Option<f64>,
    /// Cache writes. `None` bills them as fresh input.
    pub cache_write_per_million: Option<f64>,
}

impl Rates {
    /// Rates with no cache pricing.
    pub fn per_million(input: f64, output: f64) -> Self {
        Self {
            input_per_million: input,
            output_per_million: output,
            cache_read_per_million: None,
            cache_write_per_million: None,
        }
    }

    /// Free (a local model server).
    pub fn free() -> Self {
        Self::per_million(0.0, 0.0)
    }

    /// USD for `usage`.
    pub fn cost(self, usage: Usage) -> f64 {
        let read = self
            .cache_read_per_million
            .unwrap_or(self.input_per_million);
        let write = self
            .cache_write_per_million
            .unwrap_or(self.input_per_million);
        let fresh = usage
            .input_tokens
            .saturating_sub(usage.cached_tokens)
            .saturating_sub(usage.cache_write_tokens);
        (fresh as f64 * self.input_per_million
            + usage.cached_tokens as f64 * read
            + usage.cache_write_tokens as f64 * write
            + usage.output_tokens as f64 * self.output_per_million)
            / 1_000_000.0
    }
}

/// Looks up rates: TOML override → live catalog (`GET /models`).
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    overrides: HashMap<String, Rates>,
    catalog: HashMap<String, Rates>,
}

impl PriceBook {
    /// Empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load TOML `[pricing]` overrides from config.
    pub fn from_config(cfg: &Config) -> Self {
        let mut book = Self::new();
        for (model, o) in &cfg.pricing {
            book.overrides.insert(model.clone(), override_rates(o));
        }
        book
    }

    /// Merge a live `/models` list into the catalog. Newer rows replace
    /// older ones: prices change.
    pub fn ingest(&mut self, models: &[ModelInfo]) -> usize {
        let mut n = 0;
        for m in models {
            let (Some(input), Some(output)) = (m.input_per_million, m.output_per_million) else {
                continue;
            };
            self.catalog.insert(
                m.id.clone(),
                Rates {
                    input_per_million: input,
                    output_per_million: output,
                    cache_read_per_million: m.cache_read_per_million,
                    cache_write_per_million: m.cache_write_per_million,
                },
            );
            n += 1;
        }
        n
    }

    /// Keep another book's catalog rows (not its overrides) where this one
    /// has none.
    pub fn absorb(&mut self, other: &PriceBook) {
        for (k, v) in &other.catalog {
            self.catalog.entry(k.clone()).or_insert(*v);
        }
    }

    /// Rates for `model`, if known.
    pub fn rates(&self, model: &str) -> Option<Rates> {
        self.overrides
            .get(model)
            .or_else(|| self.catalog.get(model))
            .copied()
    }

    /// USD cost, or `None` when the model has no rates (`$?.??`).
    ///
    /// Zero tokens with known rates is `$0.00`. Unknown rates never become `$0.00`.
    pub fn cost(&self, model: &str, usage: Usage) -> Option<f64> {
        self.rates(model).map(|r| r.cost(usage))
    }

    /// How many models have a known price.
    pub fn len(&self) -> usize {
        self.catalog.len() + self.overrides.len()
    }

    /// No prices known at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A running total that remembers whether any part of it was unpriced.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tally {
    /// Sum of the priced calls.
    pub usd: f64,
    /// At least one call had no known price, so `usd` is a lower bound.
    pub partial: bool,
    /// Calls counted.
    pub calls: u64,
    /// Tokens counted.
    pub usage: Usage,
}

impl Tally {
    /// Count one call.
    pub fn add(&mut self, usd: Option<f64>, usage: Usage) {
        match usd {
            Some(v) => self.usd += v,
            None => self.partial = true,
        }
        self.calls += 1;
        self.usage = self.usage + usage;
    }

    /// `$0.42`, `≥$0.42` when partial, `$?.??` when nothing was priced.
    pub fn label(&self) -> String {
        if self.partial && self.usd.abs() < 0.005 {
            "$?.??".into()
        } else if self.partial {
            format!("≥{}", format_usd(Some(self.usd)))
        } else {
            format_usd(Some(self.usd))
        }
    }
}

/// `$2/M in · $6/M out`, or `$?.??` when unknown.
pub fn format_rates(rates: Option<Rates>) -> String {
    match rates {
        Some(r) => format!(
            "${}/M in · ${}/M out",
            trim_rate(r.input_per_million),
            trim_rate(r.output_per_million)
        ),
        None => "$?.??/M".into(),
    }
}

/// `3`, `0.3`, `1.25`: a per-million rate without trailing zeros.
pub fn trim_rate(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// A priced total. Small amounts keep four places, since an operator's
/// calls are often fractions of a cent. Unknown is `$?.??`.
pub fn format_usd(total: Option<f64>) -> String {
    match total {
        // An empty f64 sum is -0.0; either way it prints as zero.
        Some(v) if v.abs() < 0.00005 => "$0.00".into(),
        Some(v) if v.abs() < 1.0 => format!("${v:.4}"),
        Some(v) => format!("${v:.2}"),
        None => "$?.??".into(),
    }
}

/// `12.4k`, `1.2M`.
pub fn format_tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn override_rates(o: &PriceOverride) -> Rates {
    Rates {
        input_per_million: o.input_per_million.unwrap_or(0.0),
        output_per_million: o.output_per_million.unwrap_or(0.0),
        cache_read_per_million: o.cache_read_per_million,
        cache_write_per_million: o.cache_write_per_million,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cached: u64, written: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cache_write_tokens: written,
        }
    }

    #[test]
    fn zero_never_prints_negative() {
        let empty: f64 = Vec::<f64>::new().into_iter().sum();
        assert_eq!(format_usd(Some(empty)), "$0.00");
        assert_eq!(format_usd(Some(0.0142)), "$0.0142");
        assert_eq!(format_usd(Some(4.126)), "$4.13");
        assert_eq!(format_usd(None), "$?.??");
    }

    #[test]
    fn unknown_model_is_none_not_zero() {
        let book = PriceBook::new();
        assert_eq!(book.cost("mystery", usage(100, 100, 0, 0)), None);
    }

    #[test]
    fn cache_reads_and_writes_use_their_own_rates() {
        let r = Rates {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: Some(0.3),
            cache_write_per_million: Some(3.75),
        };
        // 1M input of which 600k cached and 100k written; 300k fresh.
        let c = r.cost(usage(1_000_000, 0, 600_000, 100_000));
        let want = 0.3 * 3.0 + 0.6 * 0.3 + 0.1 * 3.75;
        assert!((c - want).abs() < 1e-9, "{c} vs {want}");
    }

    #[test]
    fn missing_cache_rates_bill_as_input() {
        let r = Rates::per_million(2.0, 6.0);
        let c = r.cost(usage(1_000_000, 1_000_000, 500_000, 0));
        assert!((c - 8.0).abs() < 1e-9, "{c}");
    }

    #[test]
    fn toml_override_wins() {
        let mut cfg = Config::default();
        cfg.pricing.insert(
            "m".into(),
            PriceOverride {
                input_per_million: Some(10.0),
                output_per_million: Some(20.0),
                ..Default::default()
            },
        );
        let mut book = PriceBook::from_config(&cfg);
        book.ingest(&[ModelInfo {
            input_per_million: Some(1.0),
            output_per_million: Some(1.0),
            ..ModelInfo::named("m")
        }]);
        let c = book.cost("m", usage(1_000_000, 1_000_000, 0, 0)).unwrap();
        assert!((c - 30.0).abs() < 1e-9);
    }

    #[test]
    fn tally_says_when_it_is_a_lower_bound() {
        let mut t = Tally::default();
        t.add(Some(0.25), Usage::default());
        assert_eq!(t.label(), "$0.2500");
        t.add(None, Usage::default());
        assert_eq!(t.label(), "≥$0.2500");
        let mut u = Tally::default();
        u.add(None, Usage::default());
        assert_eq!(u.label(), "$?.??");
    }

    #[test]
    fn tokens_are_compact() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(12_400), "12.4k");
        assert_eq!(format_tokens(1_250_000), "1.2M");
    }
}
