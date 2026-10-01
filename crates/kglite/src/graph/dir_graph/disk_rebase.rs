//! What a disk save does to the live handle once its generation is published.
//!
//! By the time `save_disk` calls in here the save has already succeeded: the
//! generation is durable and `CURRENT` names it. Everything below is an
//! *optimisation* that moves the live handle onto the published, file-backed
//! state a fresh reader would see, so the heap copies it was serving from can go.
//! A failure therefore logs and keeps the live copies — which are correct, only
//! larger — and never turns a published save into an `Err` the caller would read
//! as "nothing was written".

use std::path::Path;

use super::DirGraph;
use crate::graph::io::columns_meta::ColumnsMeta;
use crate::graph::storage::disk::id_index::IdIndexBase;
use crate::graph::storage::disk::type_index::{TypeIndexBase, TypeIndexStore};

#[cfg(test)]
thread_local! {
    static POST_PUBLISH_FAILPOINT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// Whether a test asked `stage` to fail. Compiled out of non-test builds.
#[cfg(test)]
pub(crate) fn post_publish_failpoint(stage: &str) -> bool {
    POST_PUBLISH_FAILPOINT.with(|point| point.get() == Some(stage))
}

/// Run `body` with the named post-publish stage failing, on this thread only.
#[cfg(test)]
pub(crate) fn with_failing_stage<T>(stage: &'static str, body: impl FnOnce() -> T) -> T {
    POST_PUBLISH_FAILPOINT.with(|point| point.set(Some(stage)));
    let result = body();
    POST_PUBLISH_FAILPOINT.with(|point| point.set(None));
    result
}

impl DirGraph {
    /// Re-point the column stores, the type index and the id index at the
    /// generation `published`, best effort. See the module header for why it never fails.
    ///
    /// `columns` is the column metadata the save just published, when it wrote it
    /// in this process; without it the remap reads the published file.
    pub(super) fn rebase_after_publish(&mut self, published: &Path, columns: Option<ColumnsMeta>) {
        if let Err(error) =
            crate::graph::io::file::remap_column_stores_to_generation(published, self, columns)
        {
            eprintln!(
                "warning: the save was published, but re-mapping the column stores onto it \
                 failed ({error}); continuing with the in-memory copies"
            );
        }
        if let Err(error) = self.rebase_type_indices(published) {
            eprintln!(
                "warning: the save was published, but re-mapping the type index onto it \
                 failed ({error}); continuing with the in-memory copy"
            );
        }
        if let Err(error) = self.rebase_id_indices(published) {
            eprintln!(
                "warning: the save was published, but re-mapping the id index onto it \
                 failed ({error}); continuing with the in-memory copy"
            );
        }
    }

    /// Serve `id_indices` from the published `id_indices.bin`. The heap overlay
    /// that built an index (60-90 B per id for a `General` map) is dropped for
    /// every type the file covers, and a type the writer skipped keeps its
    /// overlay entry. Installed only when the file agrees with each covered
    /// overlay entry on its id count: a file that lost or gained ids is refused,
    /// one that mapped an id to another node is not detected.
    fn rebase_id_indices(&mut self, published: &Path) -> std::io::Result<()> {
        #[cfg(test)]
        if post_publish_failpoint("rebase_id_indices") {
            return Err(std::io::Error::other("injected id-index rebase failure"));
        }
        let Some(base) = IdIndexBase::load_from(published, &self.interner)? else {
            return Ok(());
        };
        self.id_indices
            .rebase_onto(base)
            .map_err(std::io::Error::other)
    }

    /// Serve `type_indices` from the published `type_indices.bin` instead of the
    /// heap overlay that built it. The overlay holds four bytes per node of every
    /// type touched since the last load, and stayed resident after a save.
    ///
    /// Installed only when the file agrees with the live index on every type name
    /// and member count (the members themselves are not compared); the file's
    /// buckets are ascending, which a bucket appended out of creation order need
    /// not be.
    fn rebase_type_indices(&mut self, published: &Path) -> std::io::Result<()> {
        #[cfg(test)]
        if post_publish_failpoint("rebase_type_indices") {
            return Err(std::io::Error::other("injected type-index rebase failure"));
        }
        let Some(base) = TypeIndexBase::load_from(published, &self.interner)? else {
            return Ok(());
        };
        let fresh = TypeIndexStore::from_base(base);
        let agrees = fresh.len() == self.type_indices.len()
            && self
                .type_indices
                .iter()
                .all(|(name, live)| fresh.get(name).map(|nodes| nodes.len()) == Some(live.len()));
        if !agrees {
            return Err(std::io::Error::other(
                "the published type index differs from the live one",
            ));
        }
        self.type_indices = fresh;
        Ok(())
    }
}
