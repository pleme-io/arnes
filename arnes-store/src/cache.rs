//! A disk cache with a freshness window and a stale fallback.
//!
//! Fresh (younger than `max_age`) → served with no fetch. Stale or missing →
//! fetched and written back. A failed fetch serves the stale copy before it
//! gives up, so an offline start is *stale*, not *empty*. Everything under
//! the cache root is safe to delete.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A cached value that knows when it was fetched (unix seconds).
pub trait Stamped {
    fn fetched_at(&self) -> u64;
}

/// Seconds since the epoch.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What a lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit<T> {
    Fresh(T),
    Stale(T),
    Missing,
}

/// Read `path` and judge it against `max_age` at `now` (unix seconds). An
/// unreadable or unparseable file is [`Hit::Missing`].
#[must_use]
pub fn read<T: DeserializeOwned + Stamped>(path: &Path, max_age: Duration, now: u64) -> Hit<T> {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<T>(&s).ok()) else {
        return Hit::Missing;
    };
    if Duration::from_secs(now.saturating_sub(v.fetched_at())) < max_age { Hit::Fresh(v) } else { Hit::Stale(v) }
}

/// Write `value` to `path` atomically. Failures are logged: a cache that
/// cannot be written only costs the next start a fetch.
pub fn write<T: Serialize>(path: &Path, value: &T) {
    match serde_json::to_vec_pretty(value) {
        Ok(json) => {
            if let Err(e) = crate::write_atomic(path, &json) {
                tracing::debug!(path = %path.display(), error = %e, "cache: not written");
            }
        }
        Err(e) => tracing::debug!(error = %e, "cache: not serializable"),
    }
}

/// The cached value if fresh, else `fetch()` (written back), else the stale
/// copy. `path = None` (no cache root) always fetches.
///
/// # Errors
/// Only when there is neither a usable cached copy nor a successful fetch.
pub async fn load_or_refresh<T, E, F, Fut>(path: Option<&Path>, max_age: Duration, fetch: F) -> Result<T, E>
where
    T: Serialize + DeserializeOwned + Stamped,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let stale = match path.map(|p| read::<T>(p, max_age, now_secs())) {
        Some(Hit::Fresh(v)) => return Ok(v),
        Some(Hit::Stale(v)) => Some(v),
        Some(Hit::Missing) | None => None,
    };
    match fetch().await {
        Ok(fresh) => {
            if let Some(p) = path {
                write(p, &fresh);
            }
            Ok(fresh)
        }
        Err(e) => stale.ok_or(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::cell::Cell;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct V {
        at: u64,
        n: u32,
    }
    impl Stamped for V {
        fn fetched_at(&self) -> u64 {
            self.at
        }
    }

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn missing_and_garbage_are_missing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v.json");
        assert_eq!(read::<V>(&p, DAY, 0), Hit::Missing);
        std::fs::write(&p, "nope").unwrap();
        assert_eq!(read::<V>(&p, DAY, 0), Hit::Missing);
    }

    #[test]
    fn freshness_is_judged_by_the_stamp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v.json");
        write(&p, &V { at: 1000, n: 1 });
        assert_eq!(read::<V>(&p, DAY, 1000 + 86_399), Hit::Fresh(V { at: 1000, n: 1 }));
        assert_eq!(read::<V>(&p, DAY, 1000 + 86_400), Hit::Stale(V { at: 1000, n: 1 }));
    }

    #[tokio::test]
    async fn fresh_skips_the_fetch_stale_refetches_failure_falls_back() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v.json");
        let calls = Cell::new(0);

        let got: Result<V, String> = load_or_refresh(Some(&p), DAY, || async {
            calls.set(calls.get() + 1);
            Ok(V { at: now_secs(), n: 7 })
        })
        .await;
        assert_eq!(got.unwrap().n, 7);
        let again: Result<V, String> = load_or_refresh(Some(&p), DAY, || async {
            calls.set(calls.get() + 1);
            Ok(V { at: now_secs(), n: 8 })
        })
        .await;
        assert_eq!(again.unwrap().n, 7, "fresh copy served");
        assert_eq!(calls.get(), 1);

        write(&p, &V { at: 0, n: 3 });
        let fallback: Result<V, String> = load_or_refresh(Some(&p), DAY, || async { Err("offline".to_owned()) }).await;
        assert_eq!(fallback.unwrap().n, 3, "stale beats nothing");

        let none: Result<V, String> =
            load_or_refresh(Some(&d.path().join("absent.json")), DAY, || async { Err("offline".to_owned()) }).await;
        assert_eq!(none.unwrap_err(), "offline");
    }
}
