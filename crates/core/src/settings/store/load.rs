//! Reading `settings.ron` from disk: parse, in-memory v3→v4 migration, and
//! corruption quarantine. A file that fails to parse is renamed aside so the
//! bot can still boot on compile defaults.

use std::path::Path;

use tracing::warn;

use crate::settings::SettingsError;
use crate::settings::migrate::migrate_schedules_v3_to_v4;
use crate::settings::overrides::SettingsOverrides;

pub(super) fn load_or_quarantine(path: &Path) -> Result<SettingsOverrides, SettingsError> {
    match load_overrides(path) {
        Ok(Some(o)) => Ok(o),
        Ok(None) => Ok(SettingsOverrides::default()),
        Err(e) => {
            warn!(error = ?e, "settings.ron is corrupt; quarantining");
            quarantine(path)?;
            Ok(SettingsOverrides::default())
        }
    }
}

fn migrate_then_parse(body: &str) -> Result<SettingsOverrides, ron::error::SpannedError> {
    let mut raw: ron::Value = ron::from_str(body)?;
    if !migrate_schedules_v3_to_v4(&mut raw) {
        // No v3→v4 migration was needed. Parse the original body directly —
        // avoiding the Value→string→typed round-trip that breaks in RON 0.12
        // because `ron::Value::Map` re-serializes as `{...}` map syntax while
        // `SettingsOverrides` is a named struct that requires `(...)` or
        // `StructName(...)` syntax.
        return ron::from_str(body);
    }
    tracing::info!("migrated schedules v3 → v4 in-memory");
    // Migration was applied. Deserialize directly from the mutated ron::Value
    // (which acts as a serde source) rather than re-serializing to a string.
    // ron::Value uses `forward_to_deserialize_any` so serde struct/map visitors
    // work correctly, including internally-tagged Trigger enums.
    raw.into_rust::<SettingsOverrides>().map_err(|e| {
        // ron::Error → ron::error::SpannedError: attach a dummy span so the
        // call-site error type is satisfied. Migration failures are fatal at
        // startup, so the exact span is informational only.
        ron::error::SpannedError {
            code: e,
            span: ron::error::Span {
                start: ron::error::Position { line: 0, col: 0 },
                end: ron::error::Position { line: 0, col: 0 },
            },
        }
    })
}

fn load_overrides(path: &Path) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path)?;
    let parsed: SettingsOverrides = migrate_then_parse(&body)?;
    Ok(Some(parsed))
}

pub(super) async fn load_overrides_async(
    path: &Path,
) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !tokio::fs::try_exists(path).await? {
        return Ok(None);
    }
    let body = tokio::fs::read_to_string(path).await?;
    let parsed: SettingsOverrides = migrate_then_parse(&body)?;
    Ok(Some(parsed))
}

pub(super) fn quarantine(path: &Path) -> Result<(), SettingsError> {
    if !path.exists() {
        return Ok(());
    }
    let ts = chrono::Utc::now().timestamp();
    let target = path.with_extension(format!("ron.quarantine-{ts}"));
    std::fs::rename(path, &target)?;
    warn!(target = ?target, "settings.ron quarantined");
    Ok(())
}
