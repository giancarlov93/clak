//! The models' prices per token, in LiteLLM's list
//! (`model_prices_and_context_window.json`, the one ccusage reads too): input, output, the
//! cache written (for an hour, dearer) and read, and the dearer prices of a long prompt
//! (`…_above_200k_tokens`). The app reads the list from LiteLLM once a day; one comes with
//! Clak, for the models of the agents it knows, until then.

use super::Tokens;
use serde_json::Value;
use std::collections::HashMap;

/// The list that comes with Clak: LiteLLM's, for Claude, GPT, Codex and Gemini.
const BUILTIN: &str = include_str!("prices.json");

/// A price per token, for each kind of token; what a model's list doesn't say is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Rates {
    input: Option<f64>,
    output: Option<f64>,
    cache_write: Option<f64>,
    cache_write_hour: Option<f64>,
    cache_read: Option<f64>,
}

impl Rates {
    fn set(&mut self, name: &str, price: f64) -> bool {
        let slot = match name {
            "input_cost_per_token" => &mut self.input,
            "output_cost_per_token" => &mut self.output,
            "cache_creation_input_token_cost" => &mut self.cache_write,
            "cache_creation_input_token_cost_above_1hr" => &mut self.cache_write_hour,
            "cache_read_input_token_cost" => &mut self.cache_read,
            _ => return false,
        };
        *slot = Some(price);
        true
    }

    /// This tier's rates, the base's where it says nothing.
    fn over(self, base: Rates) -> Rates {
        Rates {
            input: self.input.or(base.input),
            output: self.output.or(base.output),
            cache_write: self.cache_write.or(base.cache_write),
            cache_write_hour: self.cache_write_hour.or(base.cache_write_hour),
            cache_read: self.cache_read.or(base.cache_read),
        }
    }
}

/// A model's prices: the base ones, and those above a number of prompt tokens.
#[derive(Debug, Clone, Default, PartialEq)]
struct Model {
    base: Rates,
    tiers: Vec<(u64, Rates)>,
}

/// What a request cost, in dollars, by kind of token: the cache written for an hour with
/// the rest of the cache written.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Split {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

impl Split {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.cache_write + self.cache_read
    }

    pub(crate) fn add(&mut self, other: &Split) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }
}

#[derive(Debug, Clone, Default)]
pub struct Prices {
    models: HashMap<String, Model>,
}

impl Prices {
    /// LiteLLM's list; `None` if it isn't one.
    pub fn parse(text: &str) -> Option<Prices> {
        let Value::Object(entries) = serde_json::from_str(text).ok()? else {
            return None;
        };
        let mut models = HashMap::new();
        for (name, entry) in entries {
            let Value::Object(fields) = entry else {
                continue;
            };
            let mut model = Model::default();
            let mut tiers: HashMap<u64, Rates> = HashMap::new();
            for (field, value) in &fields {
                let Some(price) = value.as_f64() else {
                    continue;
                };
                if model.base.set(field, price) {
                    continue;
                }
                // `input_cost_per_token_above_200k_tokens`: the price above 200,000.
                if let Some((name, over)) = field
                    .strip_suffix("k_tokens")
                    .and_then(|f| f.rsplit_once("_above_"))
                    && let Ok(thousands) = over.parse::<u64>()
                {
                    tiers.entry(thousands * 1000).or_default().set(name, price);
                }
            }
            if model.base.input.is_none() && model.base.output.is_none() {
                continue;
            }
            model.tiers = tiers
                .into_iter()
                .filter(|(_, rates)| *rates != Rates::default())
                .collect();
            model.tiers.sort_by_key(|(over, _)| *over);
            models.insert(name.to_lowercase(), model);
        }
        (!models.is_empty()).then_some(Prices { models })
    }

    /// The list that comes with Clak.
    pub fn builtin() -> Prices {
        Prices::parse(BUILTIN).unwrap_or_default()
    }

    /// How many models have a price.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// What these tokens cost with this model, in dollars; `None` if it has no price.
    pub fn cost(&self, model: &str, provider: Option<&str>, tokens: &Tokens) -> Option<f64> {
        self.split(model, provider, tokens).map(|s| s.total())
    }

    /// The same, by kind of token.
    pub fn split(&self, model: &str, provider: Option<&str>, tokens: &Tokens) -> Option<Split> {
        let model = self.find(model, provider)?;
        let prompt = tokens.prompt();
        let rates = model
            .tiers
            .iter()
            .rev()
            .find(|(over, _)| prompt > *over)
            .map_or(model.base, |(_, tier)| tier.over(model.base));
        let input = rates.input.unwrap_or(0.0);
        let cache_write = rates.cache_write.unwrap_or(input);
        Some(Split {
            input: tokens.input as f64 * input,
            output: tokens.output as f64 * rates.output.unwrap_or(0.0),
            cache_write: tokens.cache_write as f64 * cache_write
                + tokens.cache_write_hour as f64 * rates.cache_write_hour.unwrap_or(cache_write),
            cache_read: tokens.cache_read as f64 * rates.cache_read.unwrap_or(input),
        })
    }

    /// The model by its name as the agent wrote it: as it is, without the provider in front
    /// (`openrouter/…`) or the date at the end (`-20250514`), with the provider in front
    /// (`gemini/…`). Never a near name: `…-free` isn't the model without it.
    fn find(&self, model: &str, provider: Option<&str>) -> Option<&Model> {
        let model = model.trim().to_lowercase();
        // `claude-opus-5-5[1m]`: the context asked for, not a model.
        let model = model.split('[').next().unwrap_or_default().to_owned();
        let mut names = vec![model.clone()];
        if let Some((_, last)) = model.rsplit_once('/') {
            names.push(last.to_owned());
        }
        for name in names.clone() {
            if let Some(undated) = undated(&name) {
                names.push(undated);
            }
        }
        if let Some(provider) = provider.map(str::to_lowercase) {
            for name in names.clone() {
                names.push(format!("{provider}/{name}"));
            }
        }
        names.iter().find_map(|name| self.models.get(name))
    }
}

/// The name without the date at the end: `-20250514`, `@20250514`, `-2025-05-14`.
fn undated(name: &str) -> Option<String> {
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((front, date)) = name.rsplit_once(['-', '@'])
        && digits(date, 8)
    {
        return Some(front.to_owned());
    }
    let mut parts = name.rsplitn(4, '-');
    let (day, month, year, front) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (digits(year, 4) && digits(month, 2) && digits(day, 2)).then(|| front.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> Prices {
        Prices::parse(
            r#"{
                "sample_spec": { "input_cost_per_token": "0.0" },
                "claude-sonnet-9": {
                    "input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6,
                    "cache_creation_input_token_cost": 3.75e-6,
                    "cache_creation_input_token_cost_above_1hr": 6e-6,
                    "cache_read_input_token_cost": 3e-7,
                    "input_cost_per_token_above_200k_tokens": 6e-6,
                    "output_cost_per_token_above_200k_tokens": 22.5e-6,
                    "input_cost_per_token_batches": 1.5e-6
                },
                "gemini/gemini-9-pro": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-5 },
                "glm-9": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6 },
                "an-image-model": { "output_cost_per_image": 0.04 }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn each_kind_of_token_at_its_rate() {
        let tokens = Tokens {
            input: 1_000,
            output: 1_000,
            cache_write: 1_000,
            cache_write_hour: 1_000,
            cache_read: 10_000,
        };
        let split = prices().split("claude-sonnet-9", None, &tokens).unwrap();
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(split.input, 0.003));
        assert!(near(split.output, 0.015));
        assert!(near(split.cache_write, 0.00375 + 0.006));
        assert!(near(split.cache_read, 0.003));
        let cost = prices().cost("claude-sonnet-9", None, &tokens).unwrap();
        assert!(near(cost, 0.003 + 0.015 + 0.00375 + 0.006 + 0.003));
    }

    #[test]
    fn a_long_prompt_at_the_dearer_rates() {
        let tokens = Tokens {
            input: 100,
            output: 1_000,
            cache_read: 250_000,
            ..Tokens::default()
        };
        let cost = prices().cost("claude-sonnet-9", None, &tokens).unwrap();
        // Input and output dearer; the cache read at its base price, the tier says nothing.
        assert!((cost - (100.0 * 6e-6 + 1_000.0 * 22.5e-6 + 250_000.0 * 3e-7)).abs() < 1e-9);
    }

    #[test]
    fn names_as_the_agents_write_them() {
        let p = prices();
        let one = Tokens {
            input: 1,
            ..Tokens::default()
        };
        assert!(p.cost("claude-sonnet-9-20260101", None, &one).is_some());
        assert!(p.cost("Claude-Sonnet-9[1m]", None, &one).is_some());
        assert!(p.cost("anthropic/claude-sonnet-9", None, &one).is_some());
        assert!(p.cost("gemini-9-pro", Some("gemini"), &one).is_some());
        assert!(p.cost("gemini-9-pro", None, &one).is_none());
        assert!(p.cost("glm-9-free", Some("opencode"), &one).is_none());
        assert!(p.cost("an-image-model", None, &one).is_none());
        assert_eq!(undated("gpt-9-2026-01-31").as_deref(), Some("gpt-9"));
    }

    #[test]
    fn the_builtin_list_has_the_agents_models() {
        let builtin = Prices::builtin();
        assert!(builtin.len() > 20);
        let one = Tokens {
            output: 1,
            ..Tokens::default()
        };
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "gpt-5-codex",
        ] {
            assert!(builtin.cost(model, None, &one).is_some(), "{model}");
        }
    }
}
