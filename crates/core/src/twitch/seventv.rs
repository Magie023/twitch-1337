//! 7TV emote catalog + manual glossary support for AI prompt grounding.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, bail, eyre};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::{APP_USER_AGENT, settings::SettingsHandle, settings::ai::AiEmotes};

const DEFAULT_BASE_URL: &str = "https://7tv.io/v3";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Max emotes returned by the on-demand `search_emotes` tool. Independent of
/// the per-turn `max_prompt_emotes` window — the tool exists to reach the full
/// catalog without bloating every prompt.
pub const SEARCH_EMOTES_LIMIT: usize = 20;

/// Manual glossary baked into the binary at build time. Curated alongside the
/// rest of the codebase; updates ship with the binary, not as a runtime file.
pub const BAKED_GLOSSARY_TOML: &str = include_str!("../../data/7tv_emotes.toml");

/// Lazily refreshes the available 7TV catalog and builds an LLM prompt block
/// from the intersection with a manual glossary.
#[derive(Debug)]
pub struct SevenTvEmoteProvider {
    settings: SettingsHandle,
    http: reqwest::Client,
    base_url: String,
    glossary: Vec<GlossaryEmote>,
    cache: Mutex<PromptCache>,
}

/// Live-readable knobs snapshotted from [`SettingsHandle`] on each use.
struct EmotesLiveCaps {
    include_global: bool,
    refresh_interval: Duration,
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    pinned_emotes: Vec<String>,
}

#[derive(Debug, Default)]
struct PromptCache {
    last_refresh: Option<Instant>,
    emotes: Option<Vec<PromptEmote>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Glossary {
    #[serde(default)]
    emotes: Vec<GlossaryEmote>,
}

#[derive(Debug, Clone, Deserialize)]
struct GlossaryEmote {
    name: String,
    meaning: String,
    #[serde(default)]
    usage: Option<String>,
    #[serde(default)]
    avoid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptEmote {
    name: String,
    meaning: String,
    usage: Option<String>,
    avoid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvUser {
    #[serde(default)]
    emote_set: Option<SevenTvEmoteSet>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvEmoteSet {
    #[serde(default)]
    emotes: Vec<SevenTvEmote>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvEmote {
    name: String,
}

/// Read the live-tunable emote knobs from a [`SettingsHandle`].
///
/// Extracted as a free function so tests can verify live-rebinding without
/// constructing a full [`SevenTvEmoteProvider`] (which requires a live
/// `reqwest::Client`).
fn live_caps_from_handle(settings: &SettingsHandle) -> EmotesLiveCaps {
    let snap = settings.load();
    if let Some(cfg) = snap.ai.emotes.as_ref() {
        EmotesLiveCaps {
            include_global: cfg.include_global,
            refresh_interval: Duration::from_secs(cfg.refresh_interval_secs),
            max_prompt_emotes: cfg.max_prompt_emotes,
            min_baseline_emotes: cfg.min_baseline_emotes.min(cfg.max_prompt_emotes),
            pinned_emotes: cfg.pinned_emotes.clone(),
        }
    } else {
        let fallback = AiEmotes::default();
        EmotesLiveCaps {
            include_global: fallback.include_global,
            refresh_interval: Duration::from_secs(fallback.refresh_interval_secs),
            max_prompt_emotes: fallback.max_prompt_emotes,
            min_baseline_emotes: fallback.min_baseline_emotes.min(fallback.max_prompt_emotes),
            pinned_emotes: fallback.pinned_emotes,
        }
    }
}

impl SevenTvEmoteProvider {
    /// Build a provider from a [`SettingsHandle`] and a TOML glossary string.
    ///
    /// Production code passes [`BAKED_GLOSSARY_TOML`]; integration tests pass
    /// a custom fixture. The glossary is parsed eagerly so malformed TOML
    /// fails the bot at startup instead of silently disabling emotes.
    ///
    /// `base_url` is resolved once at startup from the current snapshot;
    /// changing it requires a restart. All other emote knobs are read live
    /// per-call via [`Self::live_caps`].
    pub fn new(settings: SettingsHandle, glossary_toml: &str) -> Result<Self> {
        let snap = settings.load();
        let cfg = snap
            .ai
            .emotes
            .as_ref()
            .ok_or_else(|| eyre!("emotes not configured"))?;

        let glossary: Glossary =
            toml::from_str(glossary_toml).wrap_err("Failed to parse 7TV emote glossary")?;

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("Failed to build 7TV HTTP client")?;

        let base_url = cfg
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();

        // Drop-with-warning: pins absent from the baked glossary can never
        // appear in the prompt block (they're not catalog-backed entries), so
        // surface them once at startup. Pins that ARE in the glossary but not
        // in the live channel/global catalog this turn are handled silently at
        // selection time — that's an expected, dynamic condition. The pinned
        // set is live-rebindable, so a typo added later via the dashboard is
        // caught on the next restart.
        let missing = pins_missing_from_glossary(&cfg.pinned_emotes, &glossary.emotes);
        if !missing.is_empty() {
            warn!(
                missing = ?missing,
                "pinned 7TV emotes absent from the glossary; they will never appear in the prompt block"
            );
        }

        Ok(Self {
            settings,
            http,
            base_url,
            glossary: glossary.emotes,
            cache: Mutex::new(PromptCache::default()),
        })
    }

    /// Snapshot the live-tunable knobs from the current settings.
    ///
    /// If the emotes section has somehow been removed from settings while the
    /// provider is running, falls back to compiled defaults so the provider
    /// degrades gracefully rather than panicking.
    fn live_caps(&self) -> EmotesLiveCaps {
        live_caps_from_handle(&self.settings)
    }

    /// Return a turn-specific prompt block for the Twitch channel id.
    ///
    /// The backing catalog + glossary are refreshed at most once per
    /// configured interval, but ranking happens per turn so current chat emotes
    /// and the user instruction can influence which entries the model sees.
    pub async fn prompt_block_for_turn(
        &self,
        twitch_channel_id: &str,
        instruction: &str,
        recent_chat: &str,
    ) -> Option<String> {
        let caps = self.live_caps();
        let emotes = self.prompt_emotes(twitch_channel_id, &caps).await?;
        build_prompt_block(
            &emotes,
            caps.max_prompt_emotes,
            caps.min_baseline_emotes,
            &caps.pinned_emotes,
            instruction,
            recent_chat,
        )
    }

    /// Rank the full available emote set against a free-text query for the
    /// on-demand `search_emotes` tool. Returns a chat-tool result string: up to
    /// [`SEARCH_EMOTES_LIMIT`] matches in the prompt-line format, or a graceful
    /// note when nothing matches / the catalog is unavailable. Unlike the
    /// per-turn block this ignores `max_prompt_emotes` and does not dedupe
    /// against the current turn's window.
    pub async fn search_emotes(&self, twitch_channel_id: &str, query: &str) -> String {
        let caps = self.live_caps();
        match self.prompt_emotes(twitch_channel_id, &caps).await {
            Some(emotes) => build_search_block(&emotes, query, SEARCH_EMOTES_LIMIT),
            None => "Emote catalog is unavailable right now; use only the emote codes already listed in the system prompt.".to_string(),
        }
    }

    async fn prompt_emotes(
        &self,
        twitch_channel_id: &str,
        caps: &EmotesLiveCaps,
    ) -> Option<Vec<PromptEmote>> {
        let mut cache = self.cache.lock().await;
        let now = Instant::now();

        if cache
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < caps.refresh_interval)
        {
            return cache.emotes.clone();
        }

        match self.refresh_prompt_emotes(twitch_channel_id, caps).await {
            Ok(emotes) => {
                cache.last_refresh = Some(now);
                cache.emotes = emotes;
            }
            Err(e) => {
                cache.last_refresh = Some(now);
                warn!(
                    error = ?e,
                    "Failed to refresh 7TV emote glossary; using cached entries if available"
                );
            }
        }

        cache.emotes.clone()
    }

    async fn refresh_prompt_emotes(
        &self,
        twitch_channel_id: &str,
        caps: &EmotesLiveCaps,
    ) -> Result<Option<Vec<PromptEmote>>> {
        if self.glossary.is_empty() {
            debug!("7TV emote glossary is empty");
            return Ok(None);
        }

        let available = self.fetch_available_emotes(twitch_channel_id, caps).await?;
        let emotes = build_available_prompt_emotes(&self.glossary, &available);
        Ok(emotes)
    }

    async fn fetch_available_emotes(
        &self,
        twitch_channel_id: &str,
        caps: &EmotesLiveCaps,
    ) -> Result<HashSet<String>> {
        let mut global = Vec::new();
        let mut channel = Vec::new();
        let mut had_error = false;

        if caps.include_global {
            match self.fetch_global_emotes().await {
                Ok(emotes) => global = emotes,
                Err(e) => {
                    warn!(error = ?e, "Failed to fetch global 7TV emotes");
                    had_error = true;
                }
            }
        }

        match self.fetch_channel_emotes(twitch_channel_id).await {
            Ok(emotes) => channel = emotes,
            Err(e) => {
                warn!(
                    error = ?e,
                    twitch_channel_id,
                    "Failed to fetch channel 7TV emotes"
                );
                had_error = true;
            }
        }

        if global.is_empty() && channel.is_empty() && had_error {
            bail!("all configured 7TV catalog fetches failed");
        }

        Ok(merge_emote_sets(global, channel))
    }

    async fn fetch_global_emotes(&self) -> Result<Vec<SevenTvEmote>> {
        let url = format!("{}/emote-sets/global", self.base_url);
        let set: SevenTvEmoteSet = self.get_json(&url).await?;
        Ok(set.emotes)
    }

    async fn fetch_channel_emotes(&self, twitch_channel_id: &str) -> Result<Vec<SevenTvEmote>> {
        let url = format!("{}/users/twitch/{}", self.base_url, twitch_channel_id);
        let user: SevenTvUser = self.get_json(&url).await?;
        Ok(user.emote_set.map(|set| set.emotes).unwrap_or_default())
    }

    async fn get_json<T>(&self, url: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .wrap_err_with(|| format!("Failed to send 7TV request to {url}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("7TV request failed with status {status}: {body}");
        }

        response
            .json()
            .await
            .wrap_err_with(|| format!("Failed to parse 7TV response from {url}"))
    }
}

fn merge_emote_sets(global: Vec<SevenTvEmote>, channel: Vec<SevenTvEmote>) -> HashSet<String> {
    let mut available = HashSet::new();

    for emote in global {
        insert_available(&mut available, emote);
    }
    for emote in channel {
        insert_available(&mut available, emote);
    }

    available
}

fn insert_available(available: &mut HashSet<String>, emote: SevenTvEmote) {
    if emote.name.trim().is_empty() {
        return;
    }

    available.insert(emote.name);
}

fn build_available_prompt_emotes(
    glossary: &[GlossaryEmote],
    available: &HashSet<String>,
) -> Option<Vec<PromptEmote>> {
    let mut seen = HashSet::new();
    let mut emotes = Vec::new();
    let mut stale_count = 0usize;

    for emote in glossary {
        let name = emote.name.trim();
        if name.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }

        if !available.contains(name) {
            stale_count += 1;
            continue;
        }

        let meaning = normalize_prompt_field(&emote.meaning);
        if meaning.is_empty() {
            warn!(
                emote = name,
                "Skipping 7TV emote glossary entry with empty meaning"
            );
            continue;
        }

        let usage = emote
            .usage
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty());
        let avoid = emote
            .avoid
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty());
        emotes.push(PromptEmote {
            name: name.to_string(),
            meaning,
            usage,
            avoid,
        });
    }

    if stale_count > 0 {
        debug!(
            missing_count = stale_count,
            "7TV emote glossary contains entries not present in the loaded catalog"
        );
    }

    if emotes.is_empty() {
        return None;
    }

    Some(emotes)
}

fn build_prompt_block(
    emotes: &[PromptEmote],
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    pinned_emotes: &[String],
    instruction: &str,
    recent_chat: &str,
) -> Option<String> {
    let lines = select_prompt_emotes(
        emotes,
        max_prompt_emotes,
        min_baseline_emotes,
        pinned_emotes,
        instruction,
        recent_chat,
    )
    .into_iter()
    .map(format_prompt_emote_line)
    .collect::<Vec<_>>();

    if lines.is_empty() {
        return None;
    }

    // The "do not invent" rule is reconciled with the search_emotes tool: the
    // tool is registered whenever this block is present, so the model is told
    // it may reach for codes beyond the window via the tool — never fabricate.
    Some(format!(
        "\n\n7TV emotes available in this channel:\nIn normal casual Twitch-chat replies, include exactly one fitting emote by default. Use zero emotes only for extremely serious, administrative, fact-sensitive, or clearly unsuitable topics. Use two emotes only when the chat moment is obviously hype, chaotic, or spammy. Prefer emotes recently used by chat when they fit. Use only the exact emote codes listed below or returned by the search_emotes tool; if you want an emote for a feeling that is not shown, call search_emotes to find one. Never invent or alter emote codes.\n{}",
        lines.join("\n")
    ))
}

/// Pick which emotes to inject this turn, in three deduped layers capped by
/// `max_prompt_emotes`:
/// 1. **Pinned core** — `pinned_emotes`, in list order, for the persona's
///    reflex emotes; only those present in the available set are injected
///    (absent pins are dropped). This guarantees soul-reflex emotes are always
///    in-window when they exist in the catalog.
/// 2. **Scored** — anything seen in recent chat, or whose meaning/usage shares
///    4+ char terms with the current instruction.
/// 3. **Baseline fill** — glossary-order fallbacks, until at least
///    `min_baseline_emotes` total are present, so the model always has a
///    baseline vocabulary.
///
/// No emote appears in more than one layer (deduped by glossary index).
fn select_prompt_emotes<'a>(
    emotes: &'a [PromptEmote],
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    pinned_emotes: &[String],
    instruction: &str,
    recent_chat: &str,
) -> Vec<&'a PromptEmote> {
    if max_prompt_emotes == 0 {
        return Vec::new();
    }
    let mut picked: Vec<&PromptEmote> = Vec::with_capacity(max_prompt_emotes);
    let mut picked_indexes: HashSet<usize> = HashSet::new();

    // Layer 1: pinned core.
    for name in pinned_emotes {
        if picked.len() >= max_prompt_emotes {
            break;
        }
        if let Some((index, emote)) = emotes.iter().enumerate().find(|(_, e)| e.name == *name)
            && picked_indexes.insert(index)
        {
            picked.push(emote);
        }
    }

    // Layer 2: scored.
    let context_terms = searchable_terms(instruction);
    let scored: Vec<(usize, usize, usize, &PromptEmote)> = emotes
        .iter()
        .enumerate()
        .map(|(index, emote)| {
            let recent_count = recent_emote_count(recent_chat, &emote.name);
            let context_score = context_match_score(emote, &context_terms);
            (index, recent_count, context_score, emote)
        })
        .collect();

    let mut scoring: Vec<&(usize, usize, usize, &PromptEmote)> = scored
        .iter()
        .filter(|(_, r, c, _)| *r > 0 || *c > 0)
        .collect();
    scoring.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    for entry in &scoring {
        if picked.len() >= max_prompt_emotes {
            break;
        }
        if picked_indexes.insert(entry.0) {
            picked.push(entry.3);
        }
    }

    // Layer 3: baseline fill.
    let baseline_floor = min_baseline_emotes.min(max_prompt_emotes);
    if picked.len() < baseline_floor {
        for (index, _, _, emote) in &scored {
            if picked.len() >= baseline_floor {
                break;
            }
            if picked_indexes.insert(*index) {
                picked.push(emote);
            }
        }
    }
    picked
}

/// Pinned names absent from the glossary — these can never be catalog-backed,
/// so they are reported once at startup and skipped at selection time.
fn pins_missing_from_glossary(pinned: &[String], glossary: &[GlossaryEmote]) -> Vec<String> {
    let names: HashSet<&str> = glossary.iter().map(|g| g.name.trim()).collect();
    pinned
        .iter()
        .filter(|p| !names.contains(p.trim()))
        .cloned()
        .collect()
}

/// Render the `search_emotes` tool result: up to `limit` ranked matches in the
/// prompt-line format, or a graceful note when nothing matches.
fn build_search_block(emotes: &[PromptEmote], query: &str, limit: usize) -> String {
    let matches = select_search_emotes(emotes, query, limit);
    if matches.is_empty() {
        return format!(
            "No emotes matched {query:?}. Use only the emote codes already listed in the system prompt; do not invent codes."
        );
    }
    let lines = matches
        .into_iter()
        .map(format_prompt_emote_line)
        .collect::<Vec<_>>();
    format!(
        "7TV emotes matching {query:?} ({} shown). Use the exact codes:\n{}",
        lines.len(),
        lines.join("\n")
    )
}

/// Rank the full available set against a free-text query, returning the top
/// `limit` matches. Reuses the prompt-block term scorer (`searchable_terms` +
/// `terms_match`) over name + meaning + usage, plus a strong bonus for a
/// case-insensitive partial name match so the model can look an emote up by
/// code. Zero-score entries are dropped (graceful empty when nothing matches).
fn select_search_emotes<'a>(
    emotes: &'a [PromptEmote],
    query: &str,
    limit: usize,
) -> Vec<&'a PromptEmote> {
    let terms = searchable_terms(query);
    let query_lc = query.trim().to_lowercase();
    let mut scored: Vec<(usize, usize, &PromptEmote)> = emotes
        .iter()
        .enumerate()
        .map(|(index, emote)| (search_match_score(emote, &terms, &query_lc), index, emote))
        .filter(|(score, _, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, emote)| emote)
        .collect()
}

fn search_match_score(emote: &PromptEmote, query_terms: &[String], query_lc: &str) -> usize {
    let mut score = 0;
    if query_lc.len() >= 2 && emote.name.to_lowercase().contains(query_lc) {
        score += 10;
    }
    if !query_terms.is_empty() {
        let mut fields = emote.name.clone();
        fields.push(' ');
        fields.push_str(&emote.meaning);
        if let Some(usage) = emote.usage.as_deref() {
            fields.push(' ');
            fields.push_str(usage);
        }
        let emote_terms = searchable_terms(&fields);
        score += query_terms
            .iter()
            .filter(|query| emote_terms.iter().any(|term| terms_match(query, term)))
            .count();
    }
    score
}

fn format_prompt_emote_line(emote: &PromptEmote) -> String {
    let mut line = format!("- {}: meaning={}", emote.name, emote.meaning);
    if let Some(usage) = emote.usage.as_deref() {
        line.push_str("; use=");
        line.push_str(usage);
    }
    if let Some(avoid) = emote.avoid.as_deref() {
        line.push_str("; avoid=");
        line.push_str(avoid);
    }
    line
}

fn recent_emote_count(recent_chat: &str, emote_name: &str) -> usize {
    recent_chat
        .split_whitespace()
        .filter(|token| token_matches_emote(token, emote_name))
        .count()
}

fn token_matches_emote(token: &str, emote_name: &str) -> bool {
    token == emote_name || trim_wrapping_chat_punctuation(token) == emote_name
}

fn trim_wrapping_chat_punctuation(token: &str) -> &str {
    token.trim_matches(|c| {
        matches!(
            c,
            ',' | '.' | '!' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
}

fn context_match_score(emote: &PromptEmote, context_terms: &[String]) -> usize {
    if context_terms.is_empty() {
        return 0;
    }

    let mut fields = emote.meaning.clone();
    if let Some(usage) = emote.usage.as_deref() {
        fields.push(' ');
        fields.push_str(usage);
    }
    let emote_terms = searchable_terms(&fields);

    context_terms
        .iter()
        .filter(|query| emote_terms.iter().any(|term| terms_match(query, term)))
        .count()
}

fn searchable_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| term.len() >= 4 && !is_context_stopword(term))
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

fn is_context_stopword(term: &str) -> bool {
    matches!(
        term,
        "eine"
            | "einer"
            | "einem"
            | "einen"
            | "etwas"
            | "wenn"
            | "oder"
            | "nicht"
            | "bitte"
            | "reply"
            | "message"
            | "author"
            | "parent"
            | "user"
            | "chat"
            | "channel"
    )
}

fn terms_match(query: &str, term: &str) -> bool {
    query == term
        || (query.len() >= 5
            && term.len() >= 5
            && (query.starts_with(term) || term.starts_with(query)))
}

fn normalize_prompt_field(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_global_emote_set_response() {
        let json = serde_json::json!({
            "id": "global",
            "emotes": [
                {"id": "1", "name": "KEKW", "data": {"name": "ignored"}},
                {"id": "2", "name": "peepoHappy"}
            ]
        });

        let parsed: SevenTvEmoteSet = serde_json::from_value(json).unwrap();

        assert_eq!(parsed.emotes.len(), 2);
        assert_eq!(parsed.emotes[0].name, "KEKW");
        assert_eq!(parsed.emotes[1].name, "peepoHappy");
    }

    #[test]
    fn parses_user_response_with_missing_emote_set() {
        let json = serde_json::json!({
            "id": "user",
            "emote_set": null
        });

        let parsed: SevenTvUser = serde_json::from_value(json).unwrap();

        assert!(parsed.emote_set.is_none());
    }

    #[test]
    fn merge_deduplicates_global_and_channel_emotes() {
        let global = vec![SevenTvEmote {
            name: "KEKW".into(),
        }];
        let channel = vec![SevenTvEmote {
            name: "KEKW".into(),
        }];

        let merged = merge_emote_sets(global, channel);

        assert_eq!(merged.len(), 1);
        assert!(merged.contains("KEKW"));
    }

    #[test]
    fn prompt_contains_only_glossary_entries_available_in_catalog() {
        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen".into(),
                usage: Some("wenn etwas lustig ist".into()),
                avoid: Some("ernste Themen".into()),
            },
            GlossaryEmote {
                name: "Missing".into(),
                meaning: "not available".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![SevenTvEmote {
                name: "KEKW".into(),
            }],
            Vec::new(),
        );

        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
        let prompt = build_prompt_block(&emotes, 40, 40, &[], "", "").unwrap();

        assert!(prompt.contains("KEKW"));
        assert!(prompt.contains("meaning=lachen"));
        assert!(!prompt.contains("Missing"));
    }

    #[test]
    fn stale_glossary_entries_emit_one_debug_summary() {
        use std::sync::{Arc, Mutex};
        use tracing::{
            Event, Level, Subscriber,
            field::{Field, Visit},
        };
        use tracing_subscriber::{
            layer::{Context, Layer},
            prelude::*,
        };

        #[derive(Clone, Default)]
        struct CaptureLayer {
            events: Arc<Mutex<Vec<CapturedEvent>>>,
        }

        #[derive(Debug)]
        struct CapturedEvent {
            level: Level,
            fields: Vec<(String, String)>,
        }

        #[derive(Default)]
        struct FieldVisitor {
            fields: Vec<(String, String)>,
        }

        impl Visit for FieldVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.fields
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }

        impl<S> Layer<S> for CaptureLayer
        where
            S: Subscriber,
        {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                let mut visitor = FieldVisitor::default();
                event.record(&mut visitor);
                self.events.lock().unwrap().push(CapturedEvent {
                    level: *event.metadata().level(),
                    fields: visitor.fields,
                });
            }
        }

        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "laughter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "MissingA".into(),
                meaning: "missing".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "MissingB".into(),
                meaning: "missing".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![SevenTvEmote {
                name: "KEKW".into(),
            }],
            Vec::new(),
        );
        let capture = CaptureLayer::default();
        let events = Arc::clone(&capture.events);
        let subscriber = tracing_subscriber::registry().with(capture);

        tracing::subscriber::with_default(subscriber, || {
            let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
            let prompt = build_prompt_block(&emotes, 40, 40, &[], "", "").unwrap();
            assert!(prompt.contains("KEKW"));
            assert!(!prompt.contains("MissingA"));
            assert!(!prompt.contains("MissingB"));
        });

        let events = events.lock().unwrap();
        let stale_events = events
            .iter()
            .filter(|event| {
                event.fields.iter().any(|(name, value)| {
                    name == "message"
                        && value.contains(
                            "7TV emote glossary contains entries not present in the loaded catalog",
                        )
                })
            })
            .collect::<Vec<_>>();

        assert_eq!(stale_events.len(), 1);
        assert_eq!(stale_events[0].level, Level::DEBUG);
        assert!(
            stale_events[0]
                .fields
                .iter()
                .any(|(name, value)| name == "missing_count" && value == "2")
        );
    }

    #[test]
    fn prompt_respects_max_prompt_emotes() {
        let glossary = vec![
            GlossaryEmote {
                name: "A".into(),
                meaning: "first".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "B".into(),
                meaning: "second".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote { name: "A".into() },
                SevenTvEmote { name: "B".into() },
            ],
            Vec::new(),
        );

        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
        let prompt = build_prompt_block(&emotes, 1, 1, &[], "", "").unwrap();

        assert!(prompt.contains("A"));
        assert!(!prompt.contains("B"));
    }

    #[test]
    fn prompt_prioritizes_emotes_seen_in_recent_chat() {
        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen".into(),
                usage: Some("wenn etwas lustig ist".into()),
                avoid: None,
            },
            GlossaryEmote {
                name: "LocalEmote".into(),
                meaning: "lokaler Channel-Insider".into(),
                usage: Some("wenn der Chat den Insider anspricht".into()),
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "KEKW".into(),
                },
                SevenTvEmote {
                    name: "LocalEmote".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        let prompt = build_prompt_block(
            &emotes,
            2,
            2,
            &[],
            "sag etwas lustiges",
            "## Recent chat (#main)\n[13:37] bob: LocalEmote",
        )
        .unwrap();

        let local_pos = prompt.find("- LocalEmote:").unwrap();
        let kekw_pos = prompt.find("- KEKW:").unwrap();
        assert!(
            local_pos < kekw_pos,
            "recent chat emote should rank first:\n{prompt}"
        );
    }

    #[test]
    fn prompt_prioritizes_context_matches_when_chat_is_neutral() {
        let glossary = vec![
            GlossaryEmote {
                name: "LocalEmote".into(),
                meaning: "lokaler Channel-Insider".into(),
                usage: Some("wenn der Chat den Insider anspricht".into()),
                avoid: None,
            },
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen, etwas ist lustig".into(),
                usage: Some("bei Witzen oder Fail-Momenten".into()),
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "LocalEmote".into(),
                },
                SevenTvEmote {
                    name: "KEKW".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        let prompt = build_prompt_block(&emotes, 2, 2, &[], "sag etwas lustiges", "").unwrap();

        let local_pos = prompt.find("- LocalEmote:").unwrap();
        let kekw_pos = prompt.find("- KEKW:").unwrap();
        assert!(
            kekw_pos < local_pos,
            "context-matching emote should outrank TOML order:\n{prompt}"
        );
    }

    #[test]
    fn prompt_drops_zero_score_emotes_when_baseline_is_zero() {
        let glossary = vec![
            GlossaryEmote {
                name: "Hit".into(),
                meaning: "lustig".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Idle".into(),
                meaning: "nichts dergleichen".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote { name: "Hit".into() },
                SevenTvEmote {
                    name: "Idle".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        // "lustig" matches the instruction terms; "Idle" scores zero.
        let prompt = build_prompt_block(&emotes, 8, 0, &[], "etwas lustiges", "").unwrap();

        assert!(prompt.contains("- Hit:"));
        assert!(
            !prompt.contains("- Idle:"),
            "zero-score emote leaked through:\n{prompt}"
        );
    }

    #[test]
    fn prompt_baseline_floor_fills_in_glossary_order_when_no_scoring_hits() {
        let glossary = vec![
            GlossaryEmote {
                name: "First".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Second".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Third".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "First".into(),
                },
                SevenTvEmote {
                    name: "Second".into(),
                },
                SevenTvEmote {
                    name: "Third".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        // No instruction terms ≥4 chars and no recent chat → nothing scores.
        let prompt = build_prompt_block(&emotes, 8, 2, &[], "hi", "").unwrap();

        // Exactly the baseline floor, in glossary order.
        assert!(prompt.contains("- First:"));
        assert!(prompt.contains("- Second:"));
        assert!(
            !prompt.contains("- Third:"),
            "baseline floor exceeded:\n{prompt}"
        );
    }

    #[test]
    fn prompt_block_returns_none_when_max_prompt_emotes_is_zero() {
        let glossary = vec![GlossaryEmote {
            name: "A".into(),
            meaning: "x".into(),
            usage: None,
            avoid: None,
        }];
        let available = merge_emote_sets(vec![SevenTvEmote { name: "A".into() }], Vec::new());
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        assert!(build_prompt_block(&emotes, 0, 0, &[], "hi", "").is_none());
    }

    // -----------------------------------------------------------------------
    // Pinned-core tests
    // -----------------------------------------------------------------------

    fn glossary_emote(name: &str, meaning: &str) -> GlossaryEmote {
        GlossaryEmote {
            name: name.into(),
            meaning: meaning.into(),
            usage: None,
            avoid: None,
        }
    }

    fn available_prompt_emotes(glossary: &[GlossaryEmote]) -> Vec<PromptEmote> {
        let available = merge_emote_sets(
            glossary
                .iter()
                .map(|g| SevenTvEmote {
                    name: g.name.clone(),
                })
                .collect(),
            Vec::new(),
        );
        build_available_prompt_emotes(glossary, &available).unwrap()
    }

    #[test]
    fn pinned_emote_always_present_and_leads_even_without_scoring() {
        let glossary = vec![
            glossary_emote("First", "platzhalter"),
            glossary_emote("Second", "platzhalter"),
            glossary_emote("PinnedReflex", "lacht"),
        ];
        let emotes = available_prompt_emotes(&glossary);
        // No scoring hits, baseline floor = 2 → without pinning only First+Second
        // would appear. Pinning PinnedReflex must inject it anyway, first.
        let pinned = vec!["PinnedReflex".to_string()];
        let prompt = build_prompt_block(&emotes, 8, 2, &pinned, "hi", "").unwrap();

        let pinned_pos = prompt
            .find("- PinnedReflex:")
            .expect("pinned emote must be present");
        let first_pos = prompt.find("- First:").expect("baseline fill present");
        assert!(
            pinned_pos < first_pos,
            "pinned core must lead the block:\n{prompt}"
        );
    }

    #[test]
    fn pinned_emote_absent_from_catalog_is_dropped() {
        let glossary = vec![glossary_emote("First", "platzhalter")];
        let emotes = available_prompt_emotes(&glossary);
        let pinned = vec!["Ghost".to_string()]; // not in the available set
        let prompt = build_prompt_block(&emotes, 8, 1, &pinned, "hi", "").unwrap();
        assert!(
            !prompt.contains("Ghost"),
            "absent pin must be dropped:\n{prompt}"
        );
        assert!(
            prompt.contains("- First:"),
            "baseline still fills:\n{prompt}"
        );
    }

    #[test]
    fn no_emote_appears_twice_across_pinned_scored_baseline() {
        // KEKW is pinned AND matches the instruction (would also score) AND is a
        // baseline candidate — it must appear exactly once.
        let glossary = vec![
            glossary_emote("KEKW", "lustig lachen"),
            glossary_emote("Filler", "platzhalter"),
        ];
        let emotes = available_prompt_emotes(&glossary);
        let pinned = vec!["KEKW".to_string()];
        let prompt =
            build_prompt_block(&emotes, 8, 4, &pinned, "etwas lustig", "KEKW KEKW").unwrap();
        let occurrences = prompt.matches("- KEKW:").count();
        assert_eq!(occurrences, 1, "KEKW must not be duplicated:\n{prompt}");
    }

    #[test]
    fn pinned_core_capped_by_max_prompt_emotes() {
        let glossary = vec![
            glossary_emote("A", "x"),
            glossary_emote("B", "y"),
            glossary_emote("C", "z"),
        ];
        let emotes = available_prompt_emotes(&glossary);
        let pinned = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        // max=2: only the first two pins fit; the window stays capped.
        let prompt = build_prompt_block(&emotes, 2, 0, &pinned, "hi", "").unwrap();
        assert!(prompt.contains("- A:"));
        assert!(prompt.contains("- B:"));
        assert!(
            !prompt.contains("- C:"),
            "window must stay capped at 2:\n{prompt}"
        );
    }

    // -----------------------------------------------------------------------
    // pins_missing_from_glossary
    // -----------------------------------------------------------------------

    #[test]
    fn pins_missing_from_glossary_reports_only_unknown_names() {
        let glossary = vec![
            glossary_emote("PepeLa", "lacht"),
            glossary_emote("okjj", "ok"),
        ];
        let pinned = vec!["PepeLa".to_string(), "Typo".to_string(), "okjj".to_string()];
        let missing = pins_missing_from_glossary(&pinned, &glossary);
        assert_eq!(missing, vec!["Typo".to_string()]);
    }

    // -----------------------------------------------------------------------
    // search_emotes ranking
    // -----------------------------------------------------------------------

    #[test]
    fn search_ranks_name_match_above_term_match() {
        let glossary = vec![
            glossary_emote("SomethingFunny", "ein lustiger Moment"),
            glossary_emote("Pepe", "irgendwas"),
        ];
        let emotes = available_prompt_emotes(&glossary);
        // Query "pepe" hits the name of "Pepe" (bonus) but only a weak/no term
        // match on the other entry → "Pepe" ranks first.
        let ranked = select_search_emotes(&emotes, "pepe", 10);
        assert_eq!(ranked.first().map(|e| e.name.as_str()), Some("Pepe"));
    }

    #[test]
    fn search_matches_meaning_terms() {
        let glossary = vec![
            glossary_emote("KEKW", "lachen wenn etwas lustig ist"),
            glossary_emote("Sadge", "traurig sein"),
        ];
        let emotes = available_prompt_emotes(&glossary);
        let ranked = select_search_emotes(&emotes, "lustig", 10);
        let names: Vec<&str> = ranked.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"KEKW"), "meaning match expected: {names:?}");
        assert!(
            !names.contains(&"Sadge"),
            "non-match must be excluded: {names:?}"
        );
    }

    #[test]
    fn search_respects_limit() {
        let glossary: Vec<GlossaryEmote> = (0..25)
            .map(|i| glossary_emote(&format!("Laugh{i}"), "lachen lustig moment"))
            .collect();
        let emotes = available_prompt_emotes(&glossary);
        let ranked = select_search_emotes(&emotes, "lachen", SEARCH_EMOTES_LIMIT);
        assert_eq!(ranked.len(), SEARCH_EMOTES_LIMIT, "must cap at the limit");
    }

    #[test]
    fn search_block_is_graceful_when_nothing_matches() {
        let glossary = vec![glossary_emote("KEKW", "lachen")];
        let emotes = available_prompt_emotes(&glossary);
        let block = build_search_block(&emotes, "zzzzqqqq", SEARCH_EMOTES_LIMIT);
        assert!(block.contains("No emotes matched"), "got: {block}");
    }

    #[test]
    fn search_block_renders_matches_in_prompt_line_format() {
        let glossary = vec![GlossaryEmote {
            name: "KEKW".into(),
            meaning: "lachen".into(),
            usage: Some("bei witzen".into()),
            avoid: Some("ernste themen".into()),
        }];
        let emotes = available_prompt_emotes(&glossary);
        let block = build_search_block(&emotes, "lachen", SEARCH_EMOTES_LIMIT);
        assert!(block.contains("- KEKW: meaning=lachen"), "got: {block}");
        assert!(block.contains("use=bei witzen"), "got: {block}");
        assert!(block.contains("avoid=ernste themen"), "got: {block}");
    }

    // -----------------------------------------------------------------------
    // Live-rebinding tests
    // -----------------------------------------------------------------------

    /// Build a [`SettingsHandle`] with the given `max_prompt_emotes` set in the
    /// emotes section.
    fn make_emotes_handle(max_prompt_emotes: usize) -> crate::settings::SettingsHandle {
        use std::sync::Arc;

        use arc_swap::ArcSwap;

        use crate::settings::Settings;

        let mut s = Settings::compiled_defaults();
        s.ai.emotes = Some(AiEmotes {
            max_prompt_emotes,
            min_baseline_emotes: 0, // no baseline padding so count is deterministic
            ..AiEmotes::default()
        });
        Arc::new(ArcSwap::from_pointee(s))
    }

    /// Verify that [`live_caps_from_handle`] returns updated values after the
    /// handle is mutated — i.e. that emote knobs are truly live-readable.
    ///
    /// This test deliberately avoids constructing a full [`SevenTvEmoteProvider`]
    /// (which requires a TLS-enabled `reqwest::Client`) and instead calls the
    /// extracted free function directly.
    #[test]
    fn live_caps_max_prompt_emotes_respected_and_live_rebindable() {
        use std::sync::Arc;

        // Build a glossary with 5 emotes
        let glossary_toml = r#"
[[emotes]]
name = "A"
meaning = "first"

[[emotes]]
name = "B"
meaning = "second"

[[emotes]]
name = "C"
meaning = "third"

[[emotes]]
name = "D"
meaning = "fourth"

[[emotes]]
name = "E"
meaning = "fifth"
"#;

        let available = merge_emote_sets(
            vec![
                SevenTvEmote { name: "A".into() },
                SevenTvEmote { name: "B".into() },
                SevenTvEmote { name: "C".into() },
                SevenTvEmote { name: "D".into() },
                SevenTvEmote { name: "E".into() },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(
            &toml::from_str::<Glossary>(glossary_toml).unwrap().emotes,
            &available,
        )
        .unwrap();

        // --- Phase 1: max = 5, baseline = 5 → all five emotes present ---
        let handle = make_emotes_handle(5);
        let caps = live_caps_from_handle(&handle);
        assert_eq!(caps.max_prompt_emotes, 5);
        let prompt5 =
            build_prompt_block(&emotes, caps.max_prompt_emotes, 5, &[], "hi", "").unwrap();
        assert!(prompt5.contains("- A:"));
        assert!(prompt5.contains("- E:"));

        // --- Phase 2: mutate the handle; live_caps_from_handle must reflect the change ---
        let mut s = (*handle.load_full()).clone();
        s.ai.emotes.as_mut().unwrap().max_prompt_emotes = 2;
        s.ai.emotes.as_mut().unwrap().min_baseline_emotes = 2;
        handle.store(Arc::new(s));

        let caps2 = live_caps_from_handle(&handle);
        assert_eq!(caps2.max_prompt_emotes, 2);
        let prompt2 = build_prompt_block(
            &emotes,
            caps2.max_prompt_emotes,
            caps2.min_baseline_emotes,
            &[],
            "hi",
            "",
        )
        .unwrap();
        assert!(prompt2.contains("- A:"));
        assert!(prompt2.contains("- B:"));
        assert!(
            !prompt2.contains("- C:"),
            "prompt should be capped at 2:\n{prompt2}"
        );
    }
}
