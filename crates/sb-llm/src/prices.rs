//! Token estimates and the per-model price table used by `--estimate` and
//! `--max-cost`. Prices are USD per million tokens and can be overridden with
//! the setting `llm.prices` (`{"<model>": {"input": 1.0, "output": 5.0}}`).

use second_brain_kernel::Usage;
use serde_json::Value;

/// A model price in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

/// Built-in prices (Anthropic first-party list prices at the time of writing).
/// Models not listed have no built-in price; set `llm.prices` for them.
const PRICES: &[(&str, f64, f64)] = &[
    ("claude-haiku-4-5", 1.0, 5.0),
    ("claude-sonnet-4-6", 3.0, 15.0),
    ("claude-sonnet-5", 2.0, 10.0),
    ("claude-opus-4-6", 5.0, 25.0),
    ("claude-opus-4-7", 5.0, 25.0),
    ("claude-opus-4-8", 5.0, 25.0),
    ("claude-opus-5", 5.0, 25.0),
    ("claude-opus-5-5", 4.0, 20.0),
    ("claude-fable-5", 10.0, 50.0),
    ("claude-fable-5-1", 10.0, 50.0),
];

/// Output tokens assumed per call when estimating.
pub const ESTIMATED_OUTPUT_TOKENS: u64 = 700;

/// Look up a model's price: `llm.prices` first, then the built-in table.
pub fn price_for(model: &str, overrides: Option<&Value>) -> Option<Price> {
    if let Some(p) = overrides.and_then(|o| o.get(model)) {
        let input = p.get("input").and_then(Value::as_f64)?;
        let output = p.get("output").and_then(Value::as_f64)?;
        return Some(Price { input, output });
    }
    PRICES
        .iter()
        .find(|(m, _, _)| *m == model)
        .map(|(_, i, o)| Price {
            input: *i,
            output: *o,
        })
}

/// Cost of a usage record: the provider-reported cost when present, else
/// tokens times price. `None` when unknown.
pub fn usage_cost(usage: &Usage, price: Option<Price>) -> Option<f64> {
    if let Some(c) = usage.cost_usd {
        return Some(c);
    }
    price.map(|p| {
        (usage.input_tokens as f64 * p.input + usage.output_tokens as f64 * p.output) / 1_000_000.0
    })
}

/// Character-based token estimate: CJK characters count about one token each,
/// other text about four characters per token.
pub fn estimate_tokens(text: &str) -> u64 {
    let mut cjk = 0u64;
    let mut other = 0u64;
    for c in text.chars() {
        let u = c as u32;
        if (0x3000..=0x9FFF).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
            || (0xFF00..=0xFFEF).contains(&u)
        {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lookup_and_override() {
        assert_eq!(price_for("claude-haiku-4-5", None).unwrap().input, 1.0);
        assert!(price_for("unknown", None).is_none());
        let o = json!({"unknown": {"input": 0.5, "output": 1.5}});
        assert_eq!(price_for("unknown", Some(&o)).unwrap().output, 1.5);
    }

    #[test]
    fn cost_and_tokens() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            ..Default::default()
        };
        let c = usage_cost(&u, price_for("claude-haiku-4-5", None)).unwrap();
        assert!((c - 1.5).abs() < 1e-9);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("契約書"), 3);
    }
}
