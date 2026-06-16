//! Daily dreamer ritual. Spawned from `run_bot`; sleeps until [ai.dreamer].run_at,
//! rotates the transcript, and runs the dreamer LLM against every memory file
//! plus yesterday's transcript inside nonce-fenced blocks.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone as _};
use chrono_tz::Europe::Berlin;
use chrono_tz::Tz;
use eyre::Result;
use llm::{AgentOpts, AgentOutcome, LlmClient, Message, ToolChatCompletionRequest, run_agent};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::ai::memory::inject::{
    BuildOpts, FenceLabel, InvocationChannel, SubstitutionVars, build_chat_turn_context,
    fence_block, fresh_nonce, scrub_for_inject, substitute,
};
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::tools::{DreamerExecutor, DreamerExecutorOpts, dreamer_tools};
use crate::ai::memory::transcript::TranscriptWriter;
use crate::settings::SettingsHandle;

/// Resolve `run_at` on `date` to a Berlin `DateTime`, bumping forward across the
/// spring-forward DST gap (02:00–03:00 on the last Sunday in March) when the
/// requested local time does not exist. Returns `None` only if even the
/// post-gap candidate fails to resolve, which should not happen for any real
/// Berlin date.
fn resolve_berlin_run_at(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Tz>> {
    let dt = date.and_time(time);
    if let Some(resolved) = Berlin.from_local_datetime(&dt).single() {
        return Some(resolved);
    }
    // DST gap: bump forward an hour to land past the missing window.
    let bumped = dt + chrono::Duration::hours(1);
    Berlin.from_local_datetime(&bumped).single()
}

pub fn spawn_ritual(
    llm: Arc<dyn LlmClient>,
    store: MemoryStore,
    transcript: TranscriptWriter,
    settings: SettingsHandle,
    channel: String,
    shutdown: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Re-read settings on each loop iteration. This picks up live edits
            // to dreamer.{enabled, model, reasoning_effort, run_at, timeout_secs,
            // max_rounds} and behavior.max_writes_per_turn / memory.inject_byte_budget.
            let snap = settings.load();
            let ai = &snap.ai;
            if !ai.dreamer.enabled {
                // Sleep a fixed short interval and re-check; cheap way to support
                // toggling enabled on/off without restarting the bot. 5 minutes
                // matches the latency-monitor cadence and is well under any
                // realistic run_at granularity.
                drop(snap);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(300)) => continue,
                    _ = shutdown.notified() => return,
                }
            }
            let run_at = chrono::NaiveTime::parse_from_str(&ai.dreamer.run_at, "%H:%M")
                .expect("ai.dreamer.run_at validated at settings store");
            drop(snap); // do not hold a Guard across await

            let now = chrono::Utc::now().with_timezone(&Berlin);
            let target = resolve_berlin_run_at(now.date_naive(), run_at);
            let next = target.filter(|t| *t > now).unwrap_or_else(|| {
                let tomorrow = now.date_naive().succ_opt().expect("valid next day");
                resolve_berlin_run_at(tomorrow, run_at)
                    .expect("post-gap fallback resolves on the next day")
            });
            let wait = (next - now).to_std().unwrap_or_default();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.notified() => return,
            }
            let yesterday = next.date_naive().pred_opt().expect("valid prev day");
            if let Err(e) =
                run_ritual(&*llm, &store, &transcript, &settings, &channel, yesterday).await
            {
                warn!(error = ?e, "dreamer ritual failed");
            }
        }
    })
}

pub async fn run_ritual(
    llm: &dyn LlmClient,
    store: &MemoryStore,
    transcript: &TranscriptWriter,
    settings: &SettingsHandle,
    channel: &str,
    rotate_to: NaiveDate,
) -> Result<()> {
    // Snapshot once at top; build local config from live settings values.
    let snap = settings.load_full();
    let ai = &snap.ai;
    let model = ai
        .dreamer
        .model
        .clone()
        .unwrap_or_else(|| ai.connection.model.clone());
    let reasoning_effort = ai
        .dreamer
        .reasoning_effort
        .clone()
        .or_else(|| ai.connection.reasoning_effort.clone());
    let service_tier = ai
        .dreamer
        .service_tier
        .clone()
        .or_else(|| ai.connection.service_tier.clone());
    let timeout_secs = ai.dreamer.timeout_secs;
    let max_rounds = ai.dreamer.max_rounds;
    let max_writes_per_turn = ai.behavior.max_writes_per_turn;
    let inject_byte_budget = ai.memory.inject_byte_budget;
    drop(snap);

    let dated = transcript.rotate_to(rotate_to).await?;
    let transcript_text = tokio::fs::read_to_string(&dated).await.unwrap_or_default();
    let nonce = fresh_nonce();

    let mem_ctx = build_chat_turn_context(
        store,
        BuildOpts {
            inject_byte_budget,
            nonce: nonce.clone(),
            primary_history: None,
            primary_login: String::new(),
            ai_channel_history: None,
            ai_channel_login: None,
            invocation_channel: InvocationChannel::Primary,
            // Dreamer never renders recent_chat (no history buffers supplied),
            // so these are never read; pass empties to satisfy the type.
            bot_login: String::new(),
            persona_name: String::new(),
            // Dreamer has no speaker. Empty string + no history buffers
            // tells build_chat_turn_context to skip the chat-window scope
            // filter so the dreamer still sees every user file in its
            // system prompt.
            speaker_login: String::new(),
        },
    )
    .await?;
    let date_str = rotate_to.format("%Y-%m-%d").to_string();
    let transcript_block = fence_block(
        FenceLabel::Transcript { date: &date_str },
        &nonce,
        &scrub_for_inject(&transcript_text),
    );

    let dreamer_template =
        tokio::fs::read_to_string(store.prompts_dir().join("dreamer.md")).await?;
    let now_str = chrono::Utc::now()
        .with_timezone(&Berlin)
        .format("%Y-%m-%d")
        .to_string();
    let head = substitute(
        &dreamer_template,
        SubstitutionVars {
            speaker_username: "dreamer",
            speaker_display: "dreamer",
            speaker_user_id: "",
            speaker_role: "dreamer",
            channel,
            date: &now_str,
        },
    );
    // The dreamer wants every memory file in one shot, so glue durable +
    // volatile blocks back together for its system prompt. The cache-hygiene
    // split only matters for the chat-turn loop.
    let mut mem_block = mem_ctx.durable_memory;
    if !mem_ctx.volatile_state.is_empty() {
        if !mem_block.is_empty() {
            mem_block.push('\n');
        }
        mem_block.push_str(&mem_ctx.volatile_state);
    }
    let system_prompt = format!("{head}\n\n{mem_block}\n{transcript_block}");

    let exec = DreamerExecutor::new(DreamerExecutorOpts {
        store: store.clone(),
        max_writes_per_turn,
    });

    let req = ToolChatCompletionRequest {
        model,
        messages: vec![Message::system(system_prompt), Message::user("revise.")],
        tools: dreamer_tools(),
        reasoning_effort,
        service_tier,
        prior_rounds: Vec::new(),
        trace: llm::TraceIds {
            user: Some("<dreamer>".to_string()),
            session_id: Some(crate::ai::session::new_session_id()),
        },
    };
    let opts = AgentOpts {
        max_rounds,
        per_round_timeout: Some(Duration::from_secs(timeout_secs)),
    };
    match run_agent(llm, req, &exec, opts).await {
        Ok(AgentOutcome::Text(_)) => info!(rotated = %dated.display(), "dreamer ritual finished"),
        Ok(AgentOutcome::MaxRoundsExceeded) => warn!("dreamer max_rounds reached"),
        Ok(AgentOutcome::Timeout { round }) => warn!(round, "dreamer per-round timeout"),
        Err(e) => warn!(error = ?e, "dreamer llm error"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Timelike as _;

    use super::*;

    #[test]
    fn resolve_berlin_run_at_handles_dst_gap() {
        // 2026 spring-forward in Berlin is Sunday 2026-03-29; 02:00–03:00 local does not exist.
        let date = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let in_gap = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        let resolved = resolve_berlin_run_at(date, in_gap).expect("falls through past gap");
        assert_eq!(resolved.hour(), 3, "should bump into the post-DST hour");
    }

    #[test]
    fn resolve_berlin_run_at_default_safe() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let four_am = NaiveTime::from_hms_opt(4, 0, 0).unwrap();
        let resolved = resolve_berlin_run_at(date, four_am).expect("04:00 always resolves");
        assert_eq!(resolved.hour(), 4);
    }

    /// Demonstrates live config rebinding: `run_ritual` reads settings fresh on
    /// each call, so a model change stored in the `SettingsHandle` between two
    /// invocations is picked up on the second call without restarting the bot.
    ///
    /// This is a compile+logic unit test — it verifies that the model name
    /// passed to the LLM matches what is currently in the settings handle at
    /// the time `run_ritual` is called, not what was present at spawn time.
    #[tokio::test]
    async fn run_ritual_reads_settings_live() {
        use std::sync::Arc;
        use std::sync::Mutex;

        use arc_swap::ArcSwap;
        use async_trait::async_trait;
        use llm::{
            ChatCompletionRequest, LlmClient, LlmError, ToolChatCompletionRequest,
            ToolChatCompletionResponse,
        };
        use tempfile::TempDir;

        use crate::settings::{Settings, SettingsHandle};

        // ── Minimal fake LLM that records the model name from each tool call ──
        struct ModelRecorder(Mutex<Vec<String>>);
        #[async_trait]
        impl LlmClient for ModelRecorder {
            async fn chat_completion(&self, _req: ChatCompletionRequest) -> llm::Result<String> {
                Err(LlmError::Provider {
                    status: 0,
                    body: "not used".into(),
                })
            }
            async fn chat_completion_with_tools(
                &self,
                req: ToolChatCompletionRequest,
            ) -> llm::Result<ToolChatCompletionResponse> {
                self.0.lock().unwrap().push(req.model.clone());
                // Immediately terminate the agent loop.
                Ok(ToolChatCompletionResponse::Message("done".into()))
            }
        }

        // ── Wire up a temp data dir + store + transcript ──
        let dir = TempDir::new().unwrap();
        let settings: SettingsHandle =
            Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()));
        let store = crate::ai::memory::store::MemoryStore::open(dir.path(), settings.clone())
            .await
            .unwrap();
        let transcript =
            crate::ai::memory::transcript::TranscriptWriter::open(store.memories_dir())
                .await
                .unwrap();

        // The `dreamer.md` prompt template must exist or `run_ritual` errors.
        let prompts_dir = store.prompts_dir();
        tokio::fs::create_dir_all(&prompts_dir).await.unwrap();
        tokio::fs::write(
            prompts_dir.join("dreamer.md"),
            "system prompt for {channel}",
        )
        .await
        .unwrap();

        let recorder = Arc::new(ModelRecorder(Mutex::new(Vec::new())));
        let yesterday = NaiveDate::from_ymd_opt(2026, 4, 17).unwrap();

        // ── First call: dreamer.model is unset → falls back to connection.model ──
        // compiled_defaults has an empty connection.model; set it explicitly.
        {
            let mut s = (*settings.load_full()).clone();
            s.ai.connection.model = "model-v1".into();
            settings.store(Arc::new(s));
        }
        run_ritual(
            recorder.as_ref(),
            &store,
            &transcript,
            &settings,
            "testchan",
            yesterday,
        )
        .await
        .unwrap();

        // ── Second call: change model in settings (simulates a live dashboard edit) ──
        {
            let mut s = (*settings.load_full()).clone();
            s.ai.connection.model = "model-v2".into();
            settings.store(Arc::new(s));
        }
        // Use a different date so rotate_to doesn't conflict with the already-rotated file.
        let yesterday2 = NaiveDate::from_ymd_opt(2026, 4, 16).unwrap();
        run_ritual(
            recorder.as_ref(),
            &store,
            &transcript,
            &settings,
            "testchan",
            yesterday2,
        )
        .await
        .unwrap();

        let models = recorder.0.lock().unwrap().clone();
        assert_eq!(
            models.len(),
            2,
            "expected 2 LLM calls, got {}",
            models.len()
        );
        assert_eq!(models[0], "model-v1", "first call should use model-v1");
        assert_eq!(
            models[1], "model-v2",
            "second call should pick up live change to model-v2"
        );
    }
}
