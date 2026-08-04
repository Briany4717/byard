//! Durable key/value storage (RFC-0029 O5).
//!
//! ```byld
//! inject Store as store
//! on mount => store.get("last_city") ok saved => { city = saved }
//! ```
//!
//! ```text
//! store.get(key)         -> value | Unit
//! store.get(key, default) -> value | default
//! store.set(key, value)  -> Unit
//! store.remove(key)      -> Unit
//! store.keys()           -> List<Str>
//! ```
//!
//! ## Why key/value and not a database
//!
//! The apps this unblocks need settings, drafts, a last-selected city, an auth
//! token and a small cache. None of them needs a query planner, and a database
//! brings a schema story, a migration story and a heavy dependency for a
//! problem that is a few hundred lines. A `storage-sqlite` feature is the
//! escape hatch when a real dataset arrives; until then this is the honest
//! shape of the need.
//!
//! ## Why one JSON file
//!
//! Values are [`HostValue`], encoded through the same mapping an HTTP response
//! is decoded by (O3), so a persisted todo list is a **readable JSON file** a
//! developer can open, diff, hand-edit and check into a fixture. A binary
//! format would buy nothing here and cost exactly that.
//!
//! ## Durability
//!
//! Every write goes to a temporary file in the same directory and is then
//! `rename`d over the real one. On every platform Byard targets that rename is
//! atomic, so a crash mid-write leaves either the old file or the new one, and
//! never the half-written file that would take the app's state with it. The
//! alternative, writing in place, turns any power loss during a `set` into a
//! corrupt store.
//!
//! A corrupt or unreadable file loads as an **empty store plus a diagnostic**,
//! never a panic (INV-4). An app whose settings file was truncated should start
//! with default settings, not fail to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::bridge::{BoxFuture, Controller, HostValue};
use crate::cap::json;

/// The on-disk file name inside the app's data directory.
const FILE_NAME: &str = "store.json";

/// The `Store` capability (RFC-0029 O5).
///
/// Cloning shares the same cache and the same lock, so two `inject`s of
/// `Store` in different views are one store, not two views of a file racing
/// each other.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    /// Where the file lives. Resolved once at construction.
    path: PathBuf,
    /// The loaded contents, and whether they have been loaded at all.
    ///
    /// An async `Mutex`, not a `std` one: it is held across the `spawn_blocking`
    /// that writes the file, which serialises concurrent `set`s into a
    /// well-defined order instead of letting two writers race for the rename.
    /// A `std::sync::Mutex` held across an await would block a pool worker.
    state: Mutex<Option<BTreeMap<String, HostValue>>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// A store in the platform's data directory for `app_name`.
    ///
    /// macOS `~/Library/Application Support/<app>`, Linux
    /// `~/.local/share/<app>`, Windows `%APPDATA%\<app>`, resolved by the
    /// `directories` crate rather than hand-rolled, because every one of those
    /// paths has an exception nobody remembers.
    ///
    /// Falls back to a `.byard` directory under the current working directory
    /// when the OS reports no data dir (a sandbox, a service account). A store
    /// that writes somewhere is better than a capability that vanishes.
    #[must_use]
    pub fn for_app(app_name: &str) -> Self {
        let dir = directories::ProjectDirs::from("dev", "byard", app_name).map_or_else(
            || PathBuf::from(".byard").join(app_name),
            |dirs| dirs.data_dir().to_path_buf(),
        );
        Self::at(dir.join(FILE_NAME))
    }

    /// A store backed by an explicit file. Used by tests and by an app that
    /// wants its state somewhere it chose.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: path.into(),
                state: Mutex::new(None),
            }),
        }
    }

    /// The file this store persists to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

/// Reads and parses the file, or an empty map if it is absent or unreadable.
///
/// Returns the diagnostic separately rather than logging it here, so the
/// decision about where a warning goes stays with the caller.
fn load(path: &Path) -> (BTreeMap<String, HostValue>, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Absent is the ordinary first-run case, not a problem to report.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (BTreeMap::new(), None),
        Err(e) => {
            return (
                BTreeMap::new(),
                Some(format!("could not read {}: {e}", path.display())),
            );
        }
    };
    let Some(HostValue::Record(fields)) = json::parse(&text) else {
        // Truncated by a crash, hand-edited into invalid JSON, or replaced by
        // something else entirely. Starting with defaults beats failing to
        // start (INV-4).
        return (
            BTreeMap::new(),
            Some(format!(
                "{} is not a readable store; starting empty",
                path.display()
            )),
        );
    };
    (fields.into_iter().collect(), None)
}

/// Writes `map` to `path` atomically: a temporary file in the same directory,
/// then a rename.
///
/// Same directory on purpose — a rename across filesystems is not atomic and
/// on some platforms is not a rename at all.
fn store_atomically(path: &Path, map: &BTreeMap<String, HostValue>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let record = HostValue::Record(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let text = serde_json::to_string_pretty(&json::host_to_json(&record))
        .unwrap_or_else(|_| "{}".to_string());

    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

impl Store {
    /// Loads the file on first use, then serves from the cache.
    ///
    /// One read per process: a `get` is a lookup, not a syscall, so a view that
    /// reads a setting per frame costs nothing after the first.
    async fn with_state<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, HostValue>) -> T,
    ) -> (T, Option<String>) {
        let mut guard = self.inner.state.lock().await;
        let mut warning = None;
        if guard.is_none() {
            let path = self.inner.path.clone();
            // The read is blocking, so it goes to the blocking pool rather than
            // stalling an async worker (INV-12).
            let (map, note) = tokio::task::spawn_blocking(move || load(&path))
                .await
                .unwrap_or_else(|_| (BTreeMap::new(), Some("the store load task failed".into())));
            warning = note;
            *guard = Some(map);
        }
        let map = guard.as_mut().unwrap_or_else(|| unreachable!());
        (f(map), warning)
    }

    /// Mutates the cache and persists it, **holding the lock across the write**.
    ///
    /// The lock has to span both halves, not just the mutation. Releasing it
    /// before the write lets two `set`s interleave: both write the same
    /// temporary file, the first rename moves it away, and the second fails
    /// with `ENOENT` — losing a write and reporting a storage error for a
    /// perfectly ordinary pair of calls. That is what an async `Mutex` is for
    /// here; a `std` one could not be held across the `spawn_blocking`.
    async fn mutate<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, HostValue>) -> T,
    ) -> Result<T, HostValue> {
        let mut guard = self.inner.state.lock().await;
        if guard.is_none() {
            let path = self.inner.path.clone();
            let (map, note) = tokio::task::spawn_blocking(move || load(&path))
                .await
                .unwrap_or_else(|_| (BTreeMap::new(), Some("the store load task failed".into())));
            *guard = Some(map);
            if let Some(note) = note {
                // Reported, and then the store carries on empty: a truncated
                // settings file must not stop the next `set` from working.
                return Err(json::error("storage", &note));
            }
        }
        let map = guard.as_mut().unwrap_or_else(|| unreachable!());
        let out = f(map);
        let snapshot = map.clone();
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || store_atomically(&path, &snapshot))
            .await
            .map_err(|_| json::error("storage", "the store write task failed"))?
            .map_err(|e| json::error("storage", &format!("could not write the store: {e}")))?;
        Ok(out)
    }
}

impl Controller for Store {
    fn type_name(&self) -> &'static str {
        "Store"
    }

    fn invoke(
        &self,
        method: &str,
        args: Vec<HostValue>,
    ) -> BoxFuture<'static, Result<HostValue, HostValue>> {
        let this = self.clone();
        let method = method.to_string();
        let mut args = args.into_iter();
        Box::pin(async move {
            match method.as_str() {
                "get" => {
                    let Some(HostValue::Str(key)) = args.next() else {
                        return Err(json::error(
                            "bad_argument",
                            "`store.get` takes a key string",
                        ));
                    };
                    // The second argument is what a missing key resolves to.
                    // Without it every caller writes the same guard, and the
                    // one that forgets assigns `Unit` to a `List` var and finds
                    // out when `.len` renders as nothing, which is the exact
                    // shape of bug an empty first run should not be able to
                    // cause.
                    let fallback = args.next().unwrap_or(HostValue::Unit);
                    let (value, warning) = this.with_state(|map| map.get(&key).cloned()).await;
                    if let Some(warning) = warning {
                        return Err(json::error("storage", &warning));
                    }
                    // A missing key is not an error either way: "nothing saved
                    // yet" is the first run of every app that persists
                    // anything, and it is not a failure.
                    Ok(value.unwrap_or(fallback))
                }
                "set" => {
                    let Some(HostValue::Str(key)) = args.next() else {
                        return Err(json::error(
                            "bad_argument",
                            "`store.set` takes a key string and a value",
                        ));
                    };
                    let value = args.next().unwrap_or(HostValue::Unit);
                    this.mutate(|map| map.insert(key, value))
                        .await
                        .map(|_| HostValue::Unit)
                }
                "remove" => {
                    let Some(HostValue::Str(key)) = args.next() else {
                        return Err(json::error(
                            "bad_argument",
                            "`store.remove` takes a key string",
                        ));
                    };
                    this.mutate(|map| map.remove(&key))
                        .await
                        .map(|_| HostValue::Unit)
                }
                "keys" => {
                    let (keys, warning) = this
                        .with_state(|map| {
                            map.keys()
                                .map(|k| HostValue::Str(k.clone()))
                                .collect::<Vec<_>>()
                        })
                        .await;
                    if let Some(warning) = warning {
                        return Err(json::error("storage", &warning));
                    }
                    Ok(HostValue::List(keys))
                }
                other => Err(json::error(
                    "unknown_method",
                    &format!("`Store` has no method `{other}`; try get, set, remove or keys"),
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh temporary directory, removed when the test ends.
    struct TempStore {
        dir: PathBuf,
        store: Store,
    }

    impl TempStore {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "byard-store-{name}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            let store = Store::at(dir.join(FILE_NAME));
            Self { dir, store }
        }

        /// A second `Store` over the same file, standing in for a restart.
        fn reopen(&self) -> Store {
            Store::at(self.store.path())
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn call(store: &Store, method: &str, args: Vec<HostValue>) -> Result<HostValue, HostValue> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(store.invoke(method, args))
    }

    #[test]
    fn a_value_survives_a_restart() {
        // The whole point: `set`, drop every handle, open the file again, and
        // the value is still there.
        let temp = TempStore::new("restart");
        call(
            &temp.store,
            "set",
            vec![
                HostValue::Str("last_city".into()),
                HostValue::Str("Reykjavik".into()),
            ],
        )
        .expect("set");

        let reopened = temp.reopen();
        let value = call(&reopened, "get", vec![HostValue::Str("last_city".into())]).expect("get");
        assert_eq!(value, HostValue::Str("Reykjavik".into()));
    }

    #[test]
    fn a_record_round_trips_with_its_shape_intact() {
        // A persisted todo list is the motivating case, and it is a list of
        // records: the nesting has to survive, not just the scalars.
        let temp = TempStore::new("record");
        let todos = HostValue::List(vec![
            HostValue::Record(vec![
                ("id".into(), HostValue::Int(1)),
                ("text".into(), HostValue::Str("buy milk".into())),
                ("done".into(), HostValue::Bool(false)),
            ]),
            HostValue::Record(vec![
                ("id".into(), HostValue::Int(2)),
                ("text".into(), HostValue::Str("write RFC".into())),
                ("done".into(), HostValue::Bool(true)),
            ]),
        ]);
        call(
            &temp.store,
            "set",
            vec![HostValue::Str("todos".into()), todos.clone()],
        )
        .expect("set");

        let back = call(&temp.reopen(), "get", vec![HostValue::Str("todos".into())]).expect("get");
        assert_eq!(back, todos);
    }

    #[test]
    fn the_file_is_readable_json() {
        // A developer has to be able to open it. If this ever stops being true
        // the format changed, and that is a decision, not an accident.
        let temp = TempStore::new("readable");
        call(
            &temp.store,
            "set",
            vec![
                HostValue::Str("theme".into()),
                HostValue::Str("dark".into()),
            ],
        )
        .expect("set");
        let text = std::fs::read_to_string(temp.store.path()).expect("the file exists");
        assert!(text.contains("\"theme\""), "{text}");
        assert!(text.contains("\"dark\""), "{text}");
    }

    #[test]
    fn a_missing_key_resolves_to_the_default_it_was_given() {
        // The two-argument form exists so an empty first run cannot put `Unit`
        // where a `List` belongs.
        let temp = TempStore::new("default");
        let value = call(
            &temp.store,
            "get",
            vec![HostValue::Str("todos".into()), HostValue::List(Vec::new())],
        )
        .expect("get");
        assert_eq!(value, HostValue::List(Vec::new()));
    }

    #[test]
    fn a_default_does_not_override_a_stored_value() {
        let temp = TempStore::new("default-stored");
        call(
            &temp.store,
            "set",
            vec![HostValue::Str("k".into()), HostValue::Int(9)],
        )
        .expect("set");
        let value = call(
            &temp.store,
            "get",
            vec![HostValue::Str("k".into()), HostValue::Int(0)],
        )
        .expect("get");
        assert_eq!(value, HostValue::Int(9));
    }

    #[test]
    fn a_missing_key_is_unit_and_not_an_error() {
        // "Nothing saved yet" is the first run of every app that persists
        // anything.
        let temp = TempStore::new("missing");
        let value = call(&temp.store, "get", vec![HostValue::Str("nope".into())]).expect("get");
        assert_eq!(value, HostValue::Unit);
    }

    #[test]
    fn remove_takes_the_key_out_and_the_removal_persists() {
        let temp = TempStore::new("remove");
        call(
            &temp.store,
            "set",
            vec![HostValue::Str("k".into()), HostValue::Int(1)],
        )
        .expect("set");
        call(&temp.store, "remove", vec![HostValue::Str("k".into())]).expect("remove");
        let value = call(&temp.reopen(), "get", vec![HostValue::Str("k".into())]).expect("get");
        assert_eq!(value, HostValue::Unit);
    }

    #[test]
    fn keys_lists_what_is_stored() {
        let temp = TempStore::new("keys");
        for key in ["b", "a", "c"] {
            call(
                &temp.store,
                "set",
                vec![HostValue::Str(key.into()), HostValue::Int(0)],
            )
            .expect("set");
        }
        let keys = call(&temp.store, "keys", vec![]).expect("keys");
        assert_eq!(
            keys,
            HostValue::List(vec![
                HostValue::Str("a".into()),
                HostValue::Str("b".into()),
                HostValue::Str("c".into()),
            ]),
            "keys come back sorted, so a `for` over them is stable"
        );
    }

    #[test]
    fn a_corrupt_file_starts_empty_instead_of_failing_to_start() {
        // INV-4 on data the app does not control: a store truncated by a crash
        // must not take the app down with it.
        let temp = TempStore::new("corrupt");
        std::fs::create_dir_all(temp.store.path().parent().expect("parent")).expect("mkdir");
        std::fs::write(temp.store.path(), "{ this is not json").expect("write");

        let error = call(&temp.store, "get", vec![HostValue::Str("k".into())])
            .expect_err("the corruption is reported");
        assert_eq!(error.field("kind"), Some(&HostValue::Str("storage".into())));

        // And the next write recovers: the store is usable again, on defaults.
        call(
            &temp.store,
            "set",
            vec![HostValue::Str("k".into()), HostValue::Int(7)],
        )
        .expect("set after corruption");
        assert_eq!(
            call(&temp.reopen(), "get", vec![HostValue::Str("k".into())]).expect("get"),
            HostValue::Int(7)
        );
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        // The rename is what makes the write atomic; a leftover `.tmp` would
        // mean the rename did not happen and the durability claim is false.
        let temp = TempStore::new("atomic");
        call(
            &temp.store,
            "set",
            vec![HostValue::Str("k".into()), HostValue::Int(1)],
        )
        .expect("set");
        let leftovers: Vec<_> = std::fs::read_dir(temp.store.path().parent().expect("parent"))
            .expect("readdir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|e| e == "tmp")
            })
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[test]
    fn concurrent_writers_all_land() {
        // The async `Mutex` serialises the read-modify-write, so the last
        // reader sees every writer's key rather than whichever one won the
        // rename.
        let temp = TempStore::new("concurrent");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let mut tasks = Vec::new();
            for i in 0..8 {
                let store = temp.store.clone();
                tasks.push(tokio::spawn(async move {
                    store
                        .invoke(
                            "set",
                            vec![HostValue::Str(format!("k{i}")), HostValue::Int(i)],
                        )
                        .await
                }));
            }
            for task in tasks {
                task.await.expect("task").expect("set");
            }
        });

        let keys = call(&temp.reopen(), "keys", vec![]).expect("keys");
        let HostValue::List(keys) = keys else {
            panic!("expected a list");
        };
        assert_eq!(keys.len(), 8, "every writer's key survived: {keys:?}");
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_panic() {
        let temp = TempStore::new("unknown");
        assert!(call(&temp.store, "truncate", vec![]).is_err());
    }
}
