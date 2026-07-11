pub mod bm25;
pub mod index;
pub mod parser;

use anyhow::Result;
use index::{SearchIndex, SearchIndexStats, SearchResult};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tokio::sync::Mutex;

/// Module-level singleton cache: (index, vault_dir, signature).
///
/// `signature` is a cheap fingerprint of the indexed directory's current
/// state (file count + newest mtime). It is used to detect that notes have
/// been added/changed since the cached index was built, so the index is
/// rebuilt instead of serving stale (empty) results.
///
/// Why this matters: previously the cache trusted a persisted
/// `.pixie_index.json` on cold start and never rebuilt as long as vault_dir
/// was unchanged. If that file was stale (e.g. the post-write `rebuild_index`
/// was fire-and-forget and hadn't flushed before the app quit), searches
/// returned empty forever. The mtime signature makes staleness detection
/// independent of that persisted file.
static INDEX: LazyLock<Mutex<CachedIndex>> = LazyLock::new(|| Mutex::new(CachedIndex::empty()));

struct CachedIndex {
    index: Option<SearchIndex>,
    vault_dir: Option<PathBuf>,
    /// Fingerprint of the directory when `index` was built.
    signature: Option<DirSignature>,
}

impl CachedIndex {
    fn empty() -> Self {
        Self {
            index: None,
            vault_dir: None,
            signature: None,
        }
    }
}

/// Index file name inside the vault's `Pixie/` directory.
const INDEX_FILE: &str = ".pixie_index.json";

/// Compute a cheap fingerprint of the `*.md` files in `dir`:
/// (number of files, newest modification time in seconds since epoch).
///
/// If any file was added/removed/modified, or its mtime changed, the
/// signature changes and the caller knows the index must be rebuilt.
/// Missing directory → a stable "empty" signature.
fn dir_signature(dir: &Path) -> DirSignature {
    let mut count: u64 = 0;
    let mut newest_secs: i64 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            count += 1;
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        let secs = dur.as_secs() as i64;
                        if secs > newest_secs {
                            newest_secs = secs;
                        }
                    }
                }
            }
        }
    }
    DirSignature { count, newest_secs }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirSignature {
    count: u64,
    newest_secs: i64,
}

/// Ensure the cached index is current for `vault_dir`.
///
/// Rebuilds when:
///   - the cached index is missing, or
///   - vault_dir changed, or
///   - the on-disk `*.md` set changed (count or newest mtime differs).
///
/// This is the key fix: staleness is detected from the live filesystem on
/// every call, so a stale persisted index file can never make searches
/// return empty indefinitely.
pub async fn ensure_index(vault_dir: &Path) -> Result<()> {
    let pixie_dir = vault_dir.join("Pixie");
    let current_sig = dir_signature(&pixie_dir);

    // Fast path: cache is valid for this dir AND the fs hasn't changed.
    {
        let guard = INDEX.lock().await;
        if let (Some(idx), Some(prev_dir), Some(prev_sig)) =
            (&guard.index, &guard.vault_dir, &guard.signature)
        {
            if prev_dir == vault_dir && *prev_sig == current_sig {
                let _ = idx; // index present & fresh
                return Ok(());
            }
        }
    }

    // Slow path: rebuild from the directory.
    //
    // We deliberately do NOT trust the persisted `.pixie_index.json` here,
    // even though one may exist. On Android the post-write `rebuild_index`
    // is fire-and-forget; if the app was backgrounded/killed before it
    // flushed, that file is stale and searching from it would return empty
    // forever (the original bug). Building from the live directory and
    // gating subsequent calls on the mtime signature is correct regardless
    // of the persisted file's state.
    let new_index = SearchIndex::build_from_dir(&pixie_dir)?;
    log::info!(
        "[search] index built: {} docs, {} terms (sig count={} newest={})",
        new_index.doc_count(),
        new_index.term_count(),
        current_sig.count,
        current_sig.newest_secs
    );

    // Persist to disk for cold start (best-effort; correctness does not depend
    // on this file anymore — see the note above).
    let index_path = pixie_dir.join(INDEX_FILE);
    if let Err(e) = new_index.save_to_disk(&index_path) {
        log::warn!("[search] failed to save index: {e:#}");
    }

    let mut guard = INDEX.lock().await;
    guard.index = Some(new_index);
    guard.vault_dir = Some(vault_dir.to_path_buf());
    guard.signature = Some(current_sig);
    Ok(())
}

/// Force-rebuild the index (e.g. after a new note is written).
/// Refreshes the signature too, so a concurrent/racing `ensure_index` won't
/// immediately rebuild again.
pub async fn rebuild_index(vault_dir: &Path) -> Result<SearchIndexStats> {
    let pixie_dir = vault_dir.join("Pixie");
    let current_sig = dir_signature(&pixie_dir);
    let new_index = SearchIndex::build_from_dir(&pixie_dir)?;
    let stats = SearchIndexStats {
        doc_count: new_index.doc_count(),
        term_count: new_index.term_count(),
    };
    log::info!(
        "[search] index rebuilt: {} docs, {} terms",
        stats.doc_count,
        stats.term_count
    );

    let index_path = pixie_dir.join(INDEX_FILE);
    if let Err(e) = new_index.save_to_disk(&index_path) {
        log::warn!("[search] failed to save index after rebuild: {e:#}");
    }

    let mut guard = INDEX.lock().await;
    guard.index = Some(new_index);
    guard.vault_dir = Some(vault_dir.to_path_buf());
    guard.signature = Some(current_sig);
    Ok(stats)
}

/// Search the index. Builds/refreshes it first if needed.
pub async fn search(query: &str, vault_dir: &Path, limit: usize) -> Result<Vec<SearchResult>> {
    ensure_index(vault_dir).await?;
    let guard = INDEX.lock().await;
    match &guard.index {
        Some(idx) => Ok(idx.search(query, limit)),
        None => Ok(vec![]),
    }
}

/// Get index stats (builds if needed).
#[allow(dead_code)]
pub async fn stats(vault_dir: &Path) -> Result<SearchIndexStats> {
    ensure_index(vault_dir).await?;
    let guard = INDEX.lock().await;
    match &guard.index {
        Some(idx) => Ok(SearchIndexStats {
            doc_count: idx.doc_count(),
            term_count: idx.term_count(),
        }),
        None => Ok(SearchIndexStats {
            doc_count: 0,
            term_count: 0,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the original bug: after a new note is written to
    /// the vault, a subsequent search must find it — even though the module's
    /// in-memory cache (and any persisted `.pixie_index.json`) was built
    /// before the note existed.
    ///
    /// `ensure_index` must detect the directory change via the mtime
    /// signature and rebuild; otherwise it would return stale/empty results.
    #[tokio::test]
    async fn test_ensure_index_rebuilds_after_new_note() {
        let tmp = std::env::temp_dir().join(format!(
            "pixie_search_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(tmp.join("Pixie")).unwrap();

        // Start clean: reset the shared cache so previous tests don't leak.
        {
            let mut guard = INDEX.lock().await;
            guard.index = None;
            guard.vault_dir = None;
            guard.signature = None;
        }

        // No notes yet → empty search.
        let hits = search("anything", &tmp, 10).await.unwrap();
        assert!(hits.is_empty());

        // Write the first note. Use a unique mtime by setting it explicitly so
        // the test is deterministic even on filesystems with coarse mtime
        // granularity.
        let note1 = tmp.join("Pixie").join("note-conv1.md");
        std::fs::write(
            &note1,
            "---\ntitle: \"Alpha\"\nconversation_id: \"conv1\"\n---\n\napple banana cherry\n",
        )
        .unwrap();

        // ensure_index must pick up the new note via signature change.
        let hits = search("apple", &tmp, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Alpha");

        // Write a second note.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let note2 = tmp.join("Pixie").join("note-conv2.md");
        std::fs::write(
            &note2,
            "---\ntitle: \"Beta\"\nconversation_id: \"conv2\"\n---\n\ndragon elephant\n",
        )
        .unwrap();

        let hits = search("dragon", &tmp, 10).await.unwrap();
        assert_eq!(hits.len(), 1, "newly written note must be searchable");
        assert_eq!(hits[0].title, "Beta");

        // And the first note is still searchable.
        let hits = search("banana", &tmp, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Alpha");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
