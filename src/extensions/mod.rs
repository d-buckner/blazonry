// SPDX-License-Identifier: AGPL-3.0-only
//! Extensions: consumers of provider shapes. One folder per extension under
//! `extensions/`, and each folder holds everything that extension needs:
//!
//! ```text
//! extensions/calendar/
//!   mod.rs            impl Extension: the template and the vars it feeds it
//!   manifest.json     what it is, what it consumes, the settings it takes
//!   template.liquid   Liquid -> Screen JSON
//!   vars.json         sample vars for the offline dev loop (`blazonry ext`)
//!   caldav.rs         the provider this folder introduced, if any
//!   caldav.manifest.json
//! ```
//!
//! The framework here is generic: selector resolution (all | explicit pick
//! list — no tags, no expressions), snapshot merge, and rebuild dispatch.
//! Adding an extension = one folder + one line in [`registry`]. There is no
//! per-kind match arm left to update.
//!
//! Two framework services read the same kind→shape table so consumers never
//! re-derive it: [`referenced_provider_ids`] (which providers any screen
//! actually consumes — the refresh loop fetches nothing else) and
//! [`rebuild_extensions_shapes`] (rebuild only screens whose consumed shape
//! changed). Both read the manifest, so they can never disagree with each
//! other or with the admin UI.
//!
//! The core set is compiled in. A community extension is the same folder shape
//! in a crate of its own: implement [`Extension`], add it to [`registry`], and
//! it appears in the admin UI with no further change. A plugin cannot invent a
//! new provider *shape* — shapes are the [`crate::providers::Snapshot`] enum,
//! and a new one is a framework change.

use crate::providers::Provider;
use serde_json::Value;
use sqlx::SqlitePool;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

pub mod calendar;
pub mod countdown;
pub mod home_assistant;
pub mod manifest;
pub mod weather;

pub use manifest::{Cardinality, ExtensionManifest, Field, FieldKind, ProviderManifest};

pub const MIGRATE: &str = crate::providers::MIGRATE;

/// The request an extension reads, and the snapshot type inside it, come from
/// the extension API crate. The server and a compiled extension cannot disagree
/// about either, because there is one definition.
pub use blazonry_ext_api::{SnapshotInput, VarsRequest};

/// ext-api reports a failure as a plain string, because a string is what crosses
/// the ABI. The framework carries anyhow errors.
fn strict<T>(r: Result<T, String>) -> anyhow::Result<T> {
    r.map_err(|e| anyhow::anyhow!(e))
}

/// One extension kind: the template it paints and the vars it feeds it.
/// Everything else — settings, cardinality, which shape it reads — comes from
/// its manifest, so the framework can drive it without knowing the kind.
pub trait Extension: Send + Sync {
    /// The Liquid template from the extension folder. Owned, because a folder
    /// loaded at runtime reads its template off disk.
    fn template(&self) -> String;
    /// Liquid vars for one rebuild, from the snapshots on hand. An extension
    /// with no snapshot yet returns its own "waiting for data" vars rather
    /// than failing, so the screen says something true.
    fn vars(&self, req: VarsRequest)
    -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>;
}

/// A manifest paired with the code that implements it, plus the source kinds
/// the folder ships. Each extension folder contributes one entry, so the
/// framework never names a kind's internals and a folder owns everything about
/// that kind.
pub struct Plugin {
    pub manifest: ExtensionManifest,
    pub extension: Arc<dyn Extension>,
    pub providers: Vec<Arc<dyn Provider>>,
}

fn plugin(
    src: &str,
    path: &str,
    extension: Arc<dyn Extension>,
    providers: Vec<Arc<dyn Provider>>,
) -> Arc<Plugin> {
    let manifest = ExtensionManifest::parse(src, path)
        .unwrap_or_else(|e| panic!("bundled manifest is unreadable: {e}"));
    Arc::new(Plugin {
        manifest,
        extension,
        providers,
    })
}

/// The extensions one rebuild dispatch may choose from.
#[derive(Default)]
pub struct Registry {
    pub plugins: Vec<Arc<Plugin>>,
}

impl Registry {
    /// The core set: the extensions this build ships with. A community
    /// extension adds one entry here, and its providers come along with it.
    fn bundled() -> Self {
        Registry {
            plugins: vec![
                plugin(
                    calendar::MANIFEST,
                    "calendar/manifest.json",
                    Arc::new(calendar::Calendar),
                    vec![Arc::new(calendar::caldav::CalDav)],
                ),
                plugin(
                    weather::MANIFEST,
                    "weather/manifest.json",
                    Arc::new(weather::Weather),
                    vec![Arc::new(weather::open_meteo::OpenMeteo)],
                ),
                plugin(
                    home_assistant::MANIFEST,
                    "home_assistant/manifest.json",
                    Arc::new(home_assistant::HomeAssistantTab),
                    vec![Arc::new(home_assistant::home_assistant::HomeAssistant)],
                ),
                plugin(
                    countdown::MANIFEST,
                    "countdown/manifest.json",
                    Arc::new(countdown::Countdown),
                    vec![],
                ),
            ],
        }
    }

    /// The bundled set plus a host's extensions. Bundled wins a name clash, and
    /// a kind the host dropped is gone: the bundled set starts fresh each sync.
    fn merged(host: Vec<Arc<Plugin>>) -> (Self, usize) {
        let mut base = Self::bundled();
        let mut joined = 0;
        for plugin in host {
            let kind = plugin.manifest.kind.as_str();
            if base.find(kind).is_some() {
                tracing::warn!("extensions: refused '{kind}': a bundled extension owns that kind");
                continue;
            }
            base.plugins.push(plugin);
            joined += 1;
        }
        (base, joined)
    }

    pub fn find(&self, kind: &str) -> Option<Arc<Plugin>> {
        self.plugins
            .iter()
            .find(|p| p.manifest.kind == kind)
            .cloned()
    }
}

/// The one place the live registry lives.
fn slot() -> &'static RwLock<Arc<Registry>> {
    static REGISTRY: OnceLock<RwLock<Arc<Registry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Arc::new(Registry::bundled())))
}

/// The live registry: the bundled set, plus whatever the extension host reported
/// at the last sync.
pub fn registry() -> Arc<Registry> {
    slot().read().expect("registry lock").clone()
}

/// Replace the runtime part of the registry with what the host reports, and
/// return how many joined. The bundled set is rebuilt rather than kept, so a
/// kind the host dropped really does disappear. A host extension may not shadow a
/// bundled kind: the build's own extensions are the ones the tests describe.
pub fn merge_host(host: Vec<Arc<Plugin>>) -> usize {
    let (registry, joined) = Registry::merged(host);
    *slot().write().expect("registry lock") = Arc::new(registry);
    joined
}

pub fn lookup(kind: &str) -> Option<Arc<Plugin>> {
    registry().find(kind)
}

/// The local wall clock stamped into every [`VarsRequest`].
fn local_now_stamp() -> String {
    chrono::Utc::now()
        .with_timezone(&crate::providers::la())
        .to_rfc3339()
}

/// The snapshots the resolved providers published, in resolution order. A
/// provider with no snapshot yet is absent, which is how an extension sees
/// "waiting for first refresh".
async fn load_inputs(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<Vec<SnapshotInput>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    use sqlx::Row;
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut q = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT ps.provider_id, p.name, ps.shape, ps.payload, ps.fetched_at \
         FROM provider_snapshots ps JOIN providers p ON p.id = ps.provider_id \
         WHERE ps.provider_id IN ({placeholders})"
    )));
    for id in ids {
        q = q.bind(*id);
    }
    let fetched: std::collections::HashMap<i64, SnapshotInput> = q
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| {
            Ok((
                r.get(0),
                SnapshotInput {
                    provider_id: r.get(0),
                    name: r.get(1),
                    shape: r.get(2),
                    payload: serde_json::from_str(r.get(3))?,
                    fetched_at: r.get(4),
                },
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(ids
        .iter()
        .filter_map(|id| fetched.get(id).cloned())
        .collect())
}

/// The shape an extension kind consumes, if any (countdown computes from the
/// calendar date; no provider). Single source: the manifest.
pub fn consumes_shape(kind: &str) -> Option<String> {
    lookup(kind)?.manifest.consumes.clone()
}

fn unknown_kind(kind: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "unknown extension kind '{kind}' (known: {})",
        registry()
            .plugins
            .iter()
            .map(|p| p.manifest.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Check submitted settings for a kind and complete them with defaults.
/// The admin API calls this on create and on patch, so a settings typo or an
/// out-of-range number fails the request instead of sitting in the row.
/// A provider pick is checked too: every id must exist and publish the shape
/// this kind consumes.
pub async fn validate_settings(
    pool: &SqlitePool,
    kind: &str,
    settings: &Value,
) -> anyhow::Result<Value> {
    let plugin = lookup(kind).ok_or_else(|| unknown_kind(kind))?;
    let settings = plugin.manifest.validate_settings(settings)?;
    if let Some(shape) = &plugin.manifest.consumes {
        let ids = resolve_ids(pool, &plugin.manifest, &settings).await?;
        if !ids.is_empty() && !crate::providers::ids_with_shape_exist(pool, shape, &ids).await? {
            anyhow::bail!("the source you picked is not a {shape} source");
        }
    }
    Ok(settings)
}

/// Resolve an extension's provider pick list for a shape: explicit ids, or
/// all providers of the shape when unset.
pub async fn resolved_provider_ids(
    pool: &SqlitePool,
    settings: &Value,
    shape: &str,
) -> anyhow::Result<Vec<i64>> {
    if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
        let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    crate::providers::provider_ids_by_shape(pool, shape).await
}

/// The providers one extension will read, per its cardinality.
///
/// `exactly_one` takes the picked provider, else the first of the shape.
/// `one_or_more` takes the pick list, else every provider of the shape.
/// No existence check here: a create validates its pick (see [`create`]), and
/// a rebuild must still paint when a picked provider has since gone away.
async fn resolve_ids(
    pool: &SqlitePool,
    m: &ExtensionManifest,
    settings: &Value,
) -> anyhow::Result<Vec<i64>> {
    let Some(shape) = &m.consumes else {
        return Ok(Vec::new());
    };
    if m.cardinality == Some(Cardinality::ExactlyOne) {
        if let Some(id) = settings.get("provider_id").and_then(|v| v.as_i64()) {
            return Ok(vec![id]);
        }
        return Ok(crate::providers::provider_ids_by_shape(pool, shape)
            .await?
            .into_iter()
            .take(1)
            .collect());
    }
    resolved_provider_ids(pool, settings, shape).await
}

/// The set of provider ids actually referenced by existing extensions.
/// A provider no extension consumes is never refreshed — an orphan costs
/// nothing. Weather's singular `provider_id` setting is folded in here so
/// the loop sees one uniform reference set.
pub async fn referenced_provider_ids(
    pool: &SqlitePool,
) -> anyhow::Result<std::collections::HashSet<i64>> {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    for ext in all_rows(pool).await? {
        let Some(plugin) = lookup(&ext.kind) else {
            continue;
        };
        if plugin.manifest.consumes.is_none() {
            continue;
        }
        let mut ids = resolve_ids(pool, &plugin.manifest, &ext.settings).await?;
        if let Some(pid) = ext.settings.get("provider_id").and_then(|v| v.as_i64()) {
            ids.push(pid);
        }
        set.extend(ids);
    }
    Ok(set)
}

/// The one request an extension sees for one rebuild: its completed settings,
/// the snapshots its resolved providers published, and the local clock.
async fn request_for(
    pool: &SqlitePool,
    kind: &str,
    settings: Value,
    ids: &[i64],
) -> anyhow::Result<VarsRequest> {
    Ok(VarsRequest {
        kind: kind.to_string(),
        settings,
        local_now: local_now_stamp(),
        provider_ids: ids.to_vec(),
        snapshots: load_inputs(pool, ids).await?,
    })
}

/// Re-render one extension screen into the BMP cache (both panel depths).
async fn repaint(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    screen_id: i64,
) {
    for bpp in [1u16, 2u16] {
        if let Err(e) = crate::cache::render_and_cache(pool, backends, cache, screen_id, bpp).await
        {
            tracing::warn!(%e, screen = screen_id, "extension re-render failed");
        }
    }
}

/// Rebuild every extension's bound screen from current snapshots.
/// Returns the number of extensions rebuilt.
pub async fn rebuild_extensions(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
) -> anyhow::Result<usize> {
    let exts = all_rows(pool).await?;
    let mut n = 0;
    for ext in exts {
        if rebuild_extension(pool, backends, cache, ext).await? {
            n += 1
        }
    }
    Ok(n)
}

/// Rebuild only extensions consuming one of `shapes` — a weather refresh
/// never repaints the calendar, and providerless extensions (no shape) are
/// never touched here. Returns the number of extensions rebuilt.
pub async fn rebuild_extensions_shapes(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    shapes: &std::collections::HashSet<String>,
) -> anyhow::Result<usize> {
    let exts = all_rows(pool).await?;
    let mut n = 0;
    for ext in exts {
        let consumed = consumes_shape(&ext.kind);
        let Some(consumed) = consumed else { continue };
        if !shapes.contains(&consumed) {
            continue;
        }
        if rebuild_extension(pool, backends, cache, ext).await? {
            n += 1
        }
    }
    Ok(n)
}

/// Rebuild one extension's bound screen from current snapshots. Driven
/// entirely by its manifest and its [`Extension`] impl — no per-kind branch.
/// Returns whether the screen was rebuilt (false = skipped, e.g. no
/// providers resolved — the last paint stays up).
async fn rebuild_extension(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    ext: ExtRow,
) -> anyhow::Result<bool> {
    let ExtRow {
        kind,
        name,
        settings,
        screen_id,
    } = ext;
    let Some(sid) = screen_id else {
        return Ok(false);
    };
    let Some(plugin) = lookup(&kind) else {
        tracing::warn!(extension = name, kind, "unknown extension kind; skipping");
        return Ok(false);
    };
    let settings = plugin.manifest.with_defaults(&settings);
    let ids = resolve_ids(pool, &plugin.manifest, &settings).await?;
    if !ids.is_empty() || plugin.manifest.consumes.is_none() {
        let vars = plugin
            .extension
            .vars(request_for(pool, &kind, settings, &ids).await?)
            .await?;
        crate::screens::set_paint(pool, sid, &plugin.extension.template(), &vars.to_string())
            .await?;
        repaint(pool, backends, cache, sid).await;
        return Ok(true);
    }
    tracing::warn!(
        extension = name,
        "no {} providers resolved; leaving screen as-is",
        plugin.manifest.consumes.as_deref().unwrap_or("?")
    );
    Ok(false)
}

/// Repaint the screens bound to one kind. The apply step calls this, so an author
/// sees the result at apply time rather than at the next refresh.
pub async fn rebuild_kind(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    kind: &str,
) -> anyhow::Result<usize> {
    let mut n = 0;
    for ext in all_rows(pool).await? {
        if ext.kind != kind {
            continue;
        }
        if rebuild_extension(pool, backends, cache, ext).await? {
            n += 1
        }
    }
    Ok(n)
}

/// Re-render providerless extensions whose content depends on the local
/// date (countdown). Called once per local-day rollover, never per minute:
/// a screen whose content changes daily must not force a minute refresh
/// cycle (e-ink wear + battery).
pub async fn rebuild_daily(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
) -> anyhow::Result<()> {
    let exts = all_rows(pool).await?;
    for ext in exts {
        if consumes_shape(&ext.kind).is_some() {
            continue;
        }
        if ext.screen_id.is_none() {
            continue;
        };
        if let Err(e) = rebuild_extension(pool, backends, cache, ext).await {
            tracing::warn!(%e, "daily rebuild failed");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `extensions` table owner: CRUD for the admin API.
// ---------------------------------------------------------------------------

pub struct ExtRow {
    pub kind: String,
    pub name: String,
    pub settings: Value,
    pub screen_id: Option<i64>,
}

async fn all_rows(pool: &SqlitePool) -> anyhow::Result<Vec<ExtRow>> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT kind, name, settings, screen_id FROM extensions")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExtRow {
            kind: r.get(0),
            name: r.get(1),
            settings: serde_json::from_str(r.get(2)).unwrap_or(serde_json::json!({})),
            screen_id: r.get(3),
        })
        .collect())
}

pub async fn kind_of(pool: &SqlitePool, id: i64) -> anyhow::Result<String> {
    Ok(
        sqlx::query_scalar("SELECT kind FROM extensions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows =
        sqlx::query("SELECT id, kind, name, settings, screen_id FROM extensions ORDER BY id")
            .fetch_all(pool)
            .await?;
    use sqlx::Row;
    Ok(rows.into_iter().map(|r| serde_json::json!({
        "id": r.get::<i64, _>(0),
        "kind": r.get::<String, _>(1),
        "name": r.get::<String, _>(2),
        "settings": serde_json::from_str::<Value>(&r.get::<String, _>(3)).unwrap_or(Value::Null),
        "screen_id": r.get::<Option<i64>, _>(4),
    })).collect())
}

pub async fn insert(
    pool: &SqlitePool,
    kind: &str,
    name: &str,
    settings: &Value,
    screen_id: i64,
) -> anyhow::Result<i64> {
    Ok(
        sqlx::query("INSERT INTO extensions (kind, name, settings, screen_id) VALUES (?, ?, ?, ?)")
            .bind(kind)
            .bind(name)
            .bind(settings.to_string())
            .bind(screen_id)
            .execute(pool)
            .await?
            .last_insert_rowid(),
    )
}

pub async fn exists(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM extensions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?
            > 0,
    )
}

pub async fn set_name(pool: &SqlitePool, id: i64, name: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_settings(pool: &SqlitePool, id: i64, settings: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET settings = ? WHERE id = ?")
        .bind(settings.to_string())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete an extension and its bound screen (screen teardown via its owner).
pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<i64>> {
    let sid: Option<i64> = sqlx::query_scalar("SELECT screen_id FROM extensions WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .flatten();
    sqlx::query("DELETE FROM extensions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if let Some(sid) = sid {
        crate::screens::delete(pool, sid).await?;
    }
    Ok(sid)
}

/// Create an extension by kind: validate its settings and cardinality against
/// the manifest, create its screen row, and paint it from the snapshots on
/// hand. Returns (extension_id, screen_id).
pub async fn create(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    kind: &str,
    name: &str,
    settings: Value,
) -> anyhow::Result<(i64, i64)> {
    let plugin = lookup(kind).ok_or_else(|| unknown_kind(kind))?;
    let m = &plugin.manifest;
    let settings = validate_settings(pool, kind, &settings).await?;
    let ids = resolve_ids(pool, m, &settings).await?;
    if let Some(shape) = &m.consumes {
        let need = match m.cardinality {
            Some(Cardinality::ExactlyOne) => format!("a {shape} source"),
            _ => format!("at least one {shape} source"),
        };
        if ids.is_empty() {
            anyhow::bail!(
                "{} needs {need}. {}",
                m.label,
                m.setup_hint.as_deref().unwrap_or("Connect one first.")
            );
        }
    }
    let vars = plugin
        .extension
        .vars(request_for(pool, kind, settings.clone(), &ids).await?)
        .await?;
    let sid =
        crate::screens::create(pool, name, &plugin.extension.template(), &vars.to_string()).await?;
    let id = insert(pool, kind, name, &settings, sid).await?;
    // First paint of this screen only, from the snapshots already stored.
    repaint(pool, backends, cache, sid).await;
    Ok((id, sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A host extension with no behaviour: the merge rules do not depend on it.
    struct Noop;

    impl Extension for Noop {
        fn template(&self) -> String {
            "{}".to_string()
        }
        fn vars(
            &self,
            _req: VarsRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
            Box::pin(async { Ok(json!({})) })
        }
    }

    fn host_plugin(kind: &str) -> Arc<Plugin> {
        plugin(
            &json!({"kind": kind, "label": kind, "blurb": "a host extension"}).to_string(),
            "the extension host",
            Arc::new(Noop),
            vec![],
        )
    }

    #[test]
    fn a_host_extension_joins_the_bundled_set() {
        let (registry, joined) = Registry::merged(vec![host_plugin("quote")]);
        assert_eq!(joined, 1);
        assert!(registry.find("quote").is_some());
        assert!(registry.find("countdown").is_some());
    }

    #[test]
    fn a_host_extension_cannot_shadow_a_bundled_kind() {
        let (registry, joined) = Registry::merged(vec![host_plugin("countdown")]);
        assert_eq!(joined, 0);
        assert_eq!(registry.find("countdown").unwrap().providers.len(), 0);
        assert!(
            registry
                .plugins
                .iter()
                .filter(|p| p.manifest.kind == "countdown")
                .count()
                == 1
        );
    }

    #[test]
    fn a_sync_with_nothing_leaves_only_the_bundled_set() {
        // A reload that finds fewer folders must not keep the old kinds alive.
        let (registry, joined) = Registry::merged(vec![]);
        assert_eq!(joined, 0);
        assert!(registry.find("quote").is_none());
        assert!(registry.find("calendar").is_some());
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(crate::providers::MIGRATE)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn referenced_ids_exclude_orphans_and_honor_explicit_picks() {
        let pool = test_pool().await;
        let cal1 =
            crate::providers::create(&pool, "caldav", "A", &json!({"url": "http://x/"}), 3600)
                .await
                .unwrap();
        let cal2 =
            crate::providers::create(&pool, "caldav", "B", &json!({"url": "http://y/"}), 3600)
                .await
                .unwrap();
        let met = crate::providers::create(
            &pool,
            "open_meteo",
            "W",
            &json!({"latitude": 1.0, "longitude": 2.0}),
            3600,
        )
        .await
        .unwrap();

        // No extensions: nothing is referenced, even though providers exist.
        assert!(referenced_provider_ids(&pool).await.unwrap().is_empty());

        // A calendar extension with default selector references all calendar
        // providers but never the weather one.
        insert(&pool, "calendar", "Cal", &json!({}), 1)
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && refs.contains(&cal2) && !refs.contains(&met));

        // Explicit pick narrows the set; the un-picked provider is orphaned.
        set_settings(&pool, 1, &json!({"provider_ids": [cal1]}))
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && !refs.contains(&cal2));

        // Weather's singular provider_id is folded into the same set.
        insert(&pool, "weather", "Wx", &json!({"provider_id": met}), 2)
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && refs.contains(&met) && !refs.contains(&cal2));
    }
}
