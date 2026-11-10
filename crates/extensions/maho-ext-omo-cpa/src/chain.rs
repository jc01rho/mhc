//! Port of omo-cpa `src/chain.ts`. Explicit, provider-qualified fallback chains.

use crate::tier_types::{CatalogModel, Tier, TierDecision};
use std::collections::{BTreeMap, BTreeSet};

/// Two alternatives per primary family leaves useful redundancy without a wall.
pub const PRIMARY_CHAIN_LIMIT: usize = 14;
/// Six last resorts keep the escape lane diverse while bounding worst-case retries.
pub const LAST_RESORT_CHAIN_LIMIT: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierProviderNames {
    pub primary: String,
    pub last: String,
}

impl TierProviderNames {
    pub fn for_tier(&self, tier: Tier) -> &str {
        match tier {
            Tier::Primary => &self.primary,
            Tier::Last => &self.last,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenerateFallbackChainsInput<'a> {
    pub catalog: &'a [CatalogModel],
    pub decisions: &'a [TierDecision],
    pub providers: TierProviderNames,
    /// Catalog model ids for which `set_fallback_chain`-compatible output is wanted.
    pub targets: Vec<String>,
}

/// Can be handed directly to `session_settings.set_fallback_chain(target, entries)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FallbackChain {
    pub target: String,
    pub entries: Vec<String>,
}

fn descending_nullable(left: Option<u64>, right: Option<u64>) -> std::cmp::Ordering {
    let left = left.map_or(-1_i64, |value| value as i64);
    let right = right.map_or(-1_i64, |value| value as i64);
    right.cmp(&left)
}

fn lexical(left: &str, right: &str) -> std::cmp::Ordering {
    left.cmp(right)
}

/// Rank only from catalog evidence, never from a guessed vendor-name hierarchy. Larger context and
/// output limits come first, then explicit reasoning support and modality breadth. Stable
/// display-name/id tie-breaks make config diffable.
fn compare_capability(left: &CatalogModel, right: &CatalogModel) -> std::cmp::Ordering {
    descending_nullable(left.context_length, right.context_length)
        .then_with(|| descending_nullable(left.max_tokens, right.max_tokens))
        .then_with(|| {
            let left = i32::from(left.thinking == Some(true));
            let right = i32::from(right.thinking == Some(true));
            right.cmp(&left)
        })
        .then_with(|| {
            let left = modality_count(left);
            let right = modality_count(right);
            right.cmp(&left)
        })
        .then_with(|| {
            lexical(
                left.display_name.as_deref().unwrap_or(""),
                right.display_name.as_deref().unwrap_or(""),
            )
        })
        .then_with(|| lexical(&left.id, &right.id))
}

fn modality_count(model: &CatalogModel) -> usize {
    model.input_modalities.as_ref().map_or(0, Vec::len)
        + model.output_modalities.as_ref().map_or(0, Vec::len)
}

fn qualify(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

/// Fill the primary section so every family that is present gets a seat before any family gets a
/// second one, then spend the remaining budget in capability order. A flat top-N lets the two
/// families with the largest context window take every seat, so a single upstream outage skips the
/// other five families entirely.
fn select_primary(
    ranked_primary: &[&CatalogModel],
    family_of: &BTreeMap<String, Option<String>>,
    target_id: &str,
) -> Vec<String> {
    let candidates: Vec<&CatalogModel> = ranked_primary
        .iter()
        .copied()
        .filter(|model| model.id != target_id)
        .collect();
    let has_family = candidates
        .iter()
        .any(|model| family_of.get(&model.id).cloned().flatten().is_some());
    if !has_family {
        return candidates
            .iter()
            .take(PRIMARY_CHAIN_LIMIT)
            .map(|model| model.id.clone())
            .collect();
    }

    let mut buckets: BTreeMap<String, Vec<&CatalogModel>> = BTreeMap::new();
    let mut ungrouped: Vec<&CatalogModel> = Vec::new();
    for model in &candidates {
        match family_of.get(&model.id).cloned().flatten() {
            Some(family) => buckets.entry(family).or_default().push(model),
            None => ungrouped.push(model),
        }
    }

    let mut chosen: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for bucket in buckets.values() {
        if chosen.len() >= PRIMARY_CHAIN_LIMIT {
            continue;
        }
        if let Some(model) = bucket.first() {
            chosen.push(model.id.clone());
            seen.insert(model.id.clone());
        }
    }
    for model in &candidates {
        if chosen.len() >= PRIMARY_CHAIN_LIMIT {
            break;
        }
        if !seen.insert(model.id.clone()) {
            continue;
        }
        chosen.push(model.id.clone());
    }
    chosen
}

fn tier_decisions_by_id(decisions: &[TierDecision]) -> BTreeMap<String, Tier> {
    let mut tiers: BTreeMap<String, Tier> = BTreeMap::new();
    for decision in decisions {
        // A classifier should emit one row per id. If duplicated input disagrees, primary wins
        // deterministically rather than accidentally demoting a model.
        match tiers.get(&decision.id) {
            None => {
                tiers.insert(decision.id.clone(), decision.tier);
            }
            Some(Tier::Last) if decision.tier == Tier::Primary => {
                tiers.insert(decision.id.clone(), Tier::Primary);
            }
            Some(_) => {}
        }
    }
    tiers
}

/// One catalog row per classified id, keeping the strongest row for duplicates.
fn tiered_catalog(
    catalog: &[CatalogModel],
    decisions: &[TierDecision],
) -> Vec<(CatalogModel, Tier)> {
    let tier_by_id = tier_decisions_by_id(decisions);
    let mut rows_by_id: BTreeMap<&str, Vec<&CatalogModel>> = BTreeMap::new();
    for model in catalog {
        if !tier_by_id.contains_key(&model.id) {
            continue;
        }
        rows_by_id.entry(model.id.as_str()).or_default().push(model);
    }

    let mut tiered: Vec<(CatalogModel, Tier)> = Vec::new();
    for (id, mut rows) in rows_by_id {
        rows.sort_by(|left, right| compare_capability(left, right));
        if let (Some(model), Some(tier)) = (rows.first(), tier_by_id.get(id)) {
            tiered.push(((*model).clone(), *tier));
        }
    }
    tiered
}

/// Build explicit, provider-qualified fallback chains with the last-resort lane appended only after
/// every selected primary alternative. Unknown targets are omitted so every emitted selector
/// resolves against the supplied catalog.
pub fn generate_fallback_chains(input: &GenerateFallbackChainsInput<'_>) -> Vec<FallbackChain> {
    let models = tiered_catalog(input.catalog, input.decisions);
    if models.is_empty() {
        return Vec::new();
    }

    let mut ranked_primary: Vec<&CatalogModel> = models
        .iter()
        .filter(|(_, tier)| *tier == Tier::Primary)
        .map(|(model, _)| model)
        .collect();
    ranked_primary.sort_by(|left, right| compare_capability(left, right));

    let family_of: BTreeMap<String, Option<String>> = input
        .decisions
        .iter()
        .map(|decision| (decision.id.clone(), decision.family.clone()))
        .collect();

    let mut ranked_last: Vec<&CatalogModel> = models
        .iter()
        .filter(|(_, tier)| *tier == Tier::Last)
        .map(|(model, _)| model)
        .collect();
    ranked_last.sort_by(|left, right| compare_capability(left, right));

    let model_by_id: BTreeMap<&str, (&CatalogModel, Tier)> = models
        .iter()
        .map(|(model, tier)| (model.id.as_str(), (model, *tier)))
        .collect();

    let mut targets: Vec<(&CatalogModel, Tier)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for id in &input.targets {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some((model, tier)) = model_by_id.get(id.as_str()) {
            targets.push((*model, *tier));
        }
    }
    targets.sort_by(|left, right| lexical(&left.0.id, &right.0.id));

    targets
        .into_iter()
        .map(|(target, tier)| {
            let target_id = target.id.as_str();
            let primary_entries = select_primary(&ranked_primary, &family_of, target_id)
                .iter()
                .map(|id| qualify(&input.providers.primary, id))
                .collect::<Vec<_>>();
            let last_entries = ranked_last
                .iter()
                .filter(|model| model.id != target_id)
                .take(LAST_RESORT_CHAIN_LIMIT)
                .map(|model| qualify(&input.providers.last, &model.id))
                .collect::<Vec<_>>();

            FallbackChain {
                target: qualify(input.providers.for_tier(tier), target_id),
                entries: primary_entries.into_iter().chain(last_entries).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier_types::CatalogModel;

    fn model(id: &str, context: Option<u64>) -> CatalogModel {
        CatalogModel {
            context_length: context,
            ..CatalogModel::new(id)
        }
    }

    fn decision(id: &str, tier: Tier, family: Option<&str>) -> TierDecision {
        TierDecision {
            id: id.to_owned(),
            tier,
            family: family.map(str::to_owned),
            reason: String::new(),
            overridden: false,
        }
    }

    fn providers() -> TierProviderNames {
        TierProviderNames {
            primary: "cliproxyapi".into(),
            last: "cliproxyapi-last".into(),
        }
    }

    #[test]
    fn every_family_gets_a_seat_before_any_family_gets_second() {
        let mut catalog = Vec::new();
        let mut decisions = Vec::new();
        // Two huge-context families would otherwise take every seat.
        for index in 0..8 {
            let gpt = format!("gpt-{index}");
            catalog.push(model(&gpt, Some(2_000_000 - index)));
            decisions.push(decision(&gpt, Tier::Primary, Some("gpt")));
            let claude = format!("claude-{index}");
            catalog.push(model(&claude, Some(1_900_000 - index)));
            decisions.push(decision(&claude, Tier::Primary, Some("claude")));
        }
        catalog.push(model("gemini-top", Some(1000)));
        decisions.push(decision("gemini-top", Tier::Primary, Some("gemini")));

        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec!["gpt-0".to_owned()],
        });
        let entries = &chains[0].entries;
        let head: Vec<&String> = entries.iter().take(3).collect();
        assert_eq!(head[0], "cliproxyapi/claude-0");
        assert_eq!(head[1], "cliproxyapi/gemini-top");
        assert_eq!(head[2], "cliproxyapi/gpt-1");
        // The target itself never leads a chain.
        assert!(!entries.iter().any(|entry| entry == "cliproxyapi/gpt-0"));
    }

    #[test]
    fn ungrouped_models_keep_capability_order_and_the_limit() {
        let ids: Vec<String> = (0..20).map(|index| format!("m{index:02}")).collect();
        let catalog: Vec<CatalogModel> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| model(id, Some(100_000 - index as u64)))
            .collect();
        let decisions: Vec<TierDecision> = ids
            .iter()
            .map(|id| decision(id, Tier::Primary, None))
            .collect();

        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec![ids[19].clone()],
        });
        let primary: Vec<&String> = chains[0]
            .entries
            .iter()
            .filter(|entry| entry.starts_with("cliproxyapi/"))
            .collect();
        assert_eq!(primary.len(), PRIMARY_CHAIN_LIMIT);
        assert_eq!(primary[0], "cliproxyapi/m00");
    }

    #[test]
    fn the_last_resort_lane_appends_after_every_primary_alternative() {
        let catalog = vec![
            model("claude-a", Some(200_000)),
            model("claude-b", Some(100_000)),
            model("junk-1", Some(90_000)),
        ];
        let decisions = vec![
            decision("claude-a", Tier::Primary, Some("claude")),
            decision("claude-b", Tier::Primary, Some("claude")),
            decision("junk-1", Tier::Last, None),
        ];
        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec!["claude-a".to_owned()],
        });
        assert_eq!(
            chains[0].entries,
            vec![
                "cliproxyapi/claude-b".to_owned(),
                "cliproxyapi-last/junk-1".to_owned()
            ]
        );
    }

    #[test]
    fn targets_are_deduplicated_sorted_and_restricted_to_the_catalog() {
        let catalog = vec![model("claude-b", Some(1)), model("claude-a", Some(1))];
        let decisions = vec![
            decision("claude-b", Tier::Primary, Some("claude")),
            decision("claude-a", Tier::Primary, Some("claude")),
        ];
        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec![
                "claude-b".to_owned(),
                "claude-b".to_owned(),
                "absent".to_owned(),
                "claude-a".to_owned(),
            ],
        });
        let targets: Vec<&str> = chains.iter().map(|chain| chain.target.as_str()).collect();
        assert_eq!(
            targets,
            vec!["cliproxyapi/claude-a", "cliproxyapi/claude-b"]
        );
    }

    #[test]
    fn a_last_tier_target_is_qualified_with_the_last_provider() {
        let catalog = vec![model("junk", Some(1)), model("claude-a", Some(10))];
        let decisions = vec![
            decision("junk", Tier::Last, None),
            decision("claude-a", Tier::Primary, Some("claude")),
        ];
        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec!["junk".to_owned()],
        });
        assert_eq!(chains[0].target, "cliproxyapi-last/junk");
        assert_eq!(chains[0].entries, vec!["cliproxyapi/claude-a".to_owned()]);
    }

    #[test]
    fn an_empty_overlap_yields_no_chains() {
        let catalog = vec![model("claude-a", Some(1))];
        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &[],
            providers: providers(),
            targets: vec!["claude-a".to_owned()],
        });
        assert!(chains.is_empty());
    }

    #[test]
    fn duplicate_decisions_resolve_primary_first() {
        let catalog = vec![model("dup", Some(1)), model("claude-a", Some(2))];
        let decisions = vec![
            decision("dup", Tier::Last, None),
            decision("dup", Tier::Primary, None),
            decision("claude-a", Tier::Primary, Some("claude")),
        ];
        let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
            catalog: &catalog,
            decisions: &decisions,
            providers: providers(),
            targets: vec!["claude-a".to_owned()],
        });
        assert!(chains[0].entries.contains(&"cliproxyapi/dup".to_owned()));
    }
}
