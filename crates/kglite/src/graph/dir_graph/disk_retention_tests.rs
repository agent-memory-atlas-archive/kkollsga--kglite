//! Generation retention: a save keeps the new generation and a window of
//! older ones, deletes the rest, and never deletes one something in this
//! process still maps.
use super::disk_link_tests::saved_graph;
use super::disk_test_support::{current_generation, load_owned, run};
use super::DirGraph;
use crate::datatypes::Value;
use crate::graph::storage::disk::generation::{prune_generations, PruneReport};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn generation_ids(root: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = std::fs::read_dir(Path::new(root).join("generations"))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_prefix("gen_")?.parse().ok()
        })
        .collect();
    ids.sort_unstable();
    ids
}

fn generation_dir(root: &str, id: u64) -> PathBuf {
    Path::new(root)
        .join("generations")
        .join(format!("gen_{id:020}"))
}

/// Save `count` more generations, each after a `SET` so the save has work.
fn save_more(graph: &mut DirGraph, path: &str, count: usize) {
    for round in 0..count {
        run(
            graph,
            &format!("MATCH (d:Department) WHERE d.id = 1 SET d.grade = {round}"),
        );
        graph.save_disk(path).unwrap();
    }
}

#[test]
fn a_save_keeps_the_new_generation_and_the_previous_one_by_default() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    assert_eq!(generation_ids(path).len(), 1, "the first save");
    save_more(&mut graph, path, 4);
    let ids = generation_ids(path);
    let newest = *ids.last().unwrap();
    assert_eq!(
        ids,
        vec![newest - 1, newest],
        "only the current generation and the one before it remain"
    );
    assert_eq!(
        current_generation(path),
        generation_dir(path, newest),
        "CURRENT names the newest"
    );
}

/// A root whose generations `1..=count` exist and whose `CURRENT` names the last.
fn synthetic_root(dir: &Path, count: u64) -> String {
    let root = dir.join(format!("root_{count}_{}", std::process::id()));
    for id in 1..=count {
        let generation = generation_dir(root.to_str().unwrap(), id);
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("metadata.json"), b"{}").unwrap();
    }
    std::fs::write(root.join("CURRENT"), format!("gen_{count:020}\n")).unwrap();
    root.to_str().unwrap().to_string()
}

#[test]
fn the_kept_window_is_a_count_of_generations_below_the_current_one() {
    let dir = TempDir::new().unwrap();
    for (keep, expected) in [(3usize, vec![4, 5, 6, 7]), (1, vec![6, 7]), (0, vec![7])] {
        let root = synthetic_root(dir.path(), 7);
        let report = prune_generations(Path::new(&root), Some(keep));
        assert_eq!(generation_ids(&root), expected, "keep {keep}");
        assert_eq!(report.removed.len(), 7 - expected.len(), "keep {keep}");
        std::fs::remove_dir_all(&root).unwrap();
    }
    // `None` keeps every generation.
    let root = synthetic_root(dir.path(), 7);
    assert_eq!(
        prune_generations(Path::new(&root), None),
        PruneReport::default()
    );
    assert_eq!(generation_ids(&root), (1..=7).collect::<Vec<_>>());
    // A generation above the one CURRENT selects is not retention's to judge.
    std::fs::create_dir_all(generation_dir(&root, 9)).unwrap();
    prune_generations(Path::new(&root), Some(0));
    assert_eq!(generation_ids(&root), vec![7, 9]);
}

#[cfg(unix)]
#[test]
fn deleting_an_older_generation_leaves_the_files_a_newer_one_shares_with_it() {
    use super::disk_link_tests::{inode, link_count, type_files};
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let first = type_files(&current_generation(path));
    let first_generation = current_generation(path);
    let first_inodes: Vec<(String, u64)> = first
        .iter()
        .map(|(name, file)| (name.clone(), inode(file)))
        .collect();
    // Saves that touch only Department keep Employee and Office linked
    // through every generation.
    save_more(&mut graph, path, 3);
    assert!(
        !first_generation.exists(),
        "the first generation is older than the window"
    );
    let latest = type_files(&current_generation(path));
    for shared in ["Employee", "Office"] {
        let original = first_inodes
            .iter()
            .find(|(name, _)| name == shared)
            .unwrap();
        assert_eq!(
            inode(&latest[shared]),
            original.1,
            "{shared} is still the file the first generation wrote"
        );
        assert!(
            link_count(&latest[shared]) >= 2,
            "{shared} stays linked from the kept previous generation"
        );
        assert!(
            latest[shared].is_file() && std::fs::metadata(&latest[shared]).unwrap().len() > 0,
            "{shared} survived the unlink of the generation it was first written in"
        );
    }
    drop(graph);
    let mut reloaded = load_owned(path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id = 19999 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(19_999 * 3)]]
    );
    assert_eq!(
        run(&mut reloaded, "MATCH (o:Office) RETURN count(o) AS c"),
        vec![vec![Value::Int64(40)]]
    );
}

#[test]
fn a_generation_a_live_reader_maps_is_kept_until_the_reader_is_gone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut writer = saved_graph(path);
    let reader_generation = generation_dir(path, *generation_ids(path).last().unwrap());
    let mut reader = load_owned(path);

    save_more(&mut writer, path, 4);
    assert!(
        reader_generation.exists(),
        "the generation the reader maps was pruned under it"
    );
    // The reader still answers from its mappings, columns and id index alike.
    assert_eq!(
        run(
            &mut reader,
            "MATCH (e:Employee) WHERE e.id = 12345 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(12_345 * 3)]]
    );

    drop(reader);
    save_more(&mut writer, path, 1);
    assert!(
        !reader_generation.exists(),
        "with the reader gone the next save prunes its generation"
    );
    let newest = *generation_ids(path).last().unwrap();
    assert_eq!(generation_ids(path), vec![newest - 1, newest]);
}

#[test]
fn a_loaded_handle_stops_pinning_the_generation_it_loaded_once_it_has_saved_past_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    drop(saved_graph(path));
    let loaded_from = current_generation(path);
    let mut graph = load_owned(path);
    save_more(&mut graph, path, 3);
    assert!(
        !loaded_from.exists(),
        "the handle that saved past its generation still pins it"
    );
    let newest = *generation_ids(path).last().unwrap();
    assert_eq!(generation_ids(path), vec![newest - 1, newest]);
}

#[test]
fn a_bare_disk_graph_handle_and_its_clones_pin_the_generation_they_were_loaded_from() {
    use crate::graph::schema::StringInterner;
    use crate::graph::storage::disk::generation::is_pinned;
    use crate::graph::storage::disk::graph::DiskGraph;
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    drop(saved_graph(path));
    let generation = current_generation(path);
    assert!(!is_pinned(&generation));

    // No column store, id index or type index is loaded here: the handle's own
    // claim is the only one, and it covers the CSR and slot files it maps.
    let (handle, _scratch) =
        DiskGraph::load_from_dir(&generation, &mut StringInterner::new()).unwrap();
    assert!(
        is_pinned(&generation),
        "the loaded handle pins its generation"
    );
    let fork = handle.clone();
    drop(handle);
    assert!(
        is_pinned(&generation),
        "a clone keeps the pin the original had"
    );
    drop(fork);
    assert!(!is_pinned(&generation), "the last holder releases it");
}

#[test]
fn a_retained_reader_survives_a_clone_after_its_generation_was_outlived() {
    // A transaction fork clones the handle and remaps its files by path, so the
    // generation must still be there when the clone is taken.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut writer = saved_graph(path);
    let reader = load_owned(path);
    save_more(&mut writer, path, 4);
    let mut forked = reader.clone();
    assert_eq!(
        run(&mut forked, "MATCH (o:Office) RETURN count(o) AS c"),
        vec![vec![Value::Int64(40)]]
    );
}

/// What a live mapping pins, one kind at a time: a rebase that fails leaves the
/// handle serving from the generation it had, and that generation must stay.
fn a_failed_rebase_pins_the_generation_it_leaves_mapped(stage: &'static str) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let first = *generation_ids(path).last().unwrap();
    for round in 0..3 {
        run(
            &mut graph,
            &format!("MATCH (d:Department) WHERE d.id = 1 SET d.grade = {round}"),
        );
        super::with_failing_stage(stage, || graph.save_disk(path).unwrap());
        #[cfg(unix)]
        if round == 0 {
            // The index files of that save are the first generation's own, so
            // what pins the generation is a mapping of a *shared* inode — the
            // case a pruned generation must not be allowed to break.
            for file in ["id_indices.bin", "type_indices.bin"] {
                assert_eq!(
                    super::disk_link_tests::inode(&current_generation(path).join(file)),
                    super::disk_link_tests::inode(&generation_dir(path, first).join(file)),
                    "{stage}: {file} was not linked"
                );
            }
        }
    }
    assert!(
        generation_dir(path, first).exists(),
        "{stage}: generation {first} is still mapped by the handle that saved over it"
    );
    // The answer a mapping of that generation gives is still served.
    assert_eq!(
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id = 777 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(777 * 3)]]
    );
    // A save whose rebase works moves the handle off it; the next prunes it.
    save_more(&mut graph, path, 2);
    assert!(
        !generation_dir(path, first).exists(),
        "{stage}: still kept after the mapping moved on"
    );
}

#[test]
fn a_failed_id_index_rebase_pins_the_generation_the_index_still_maps() {
    a_failed_rebase_pins_the_generation_it_leaves_mapped("rebase_id_indices");
}

#[test]
fn a_failed_column_remap_pins_the_generation_the_stores_still_map() {
    a_failed_rebase_pins_the_generation_it_leaves_mapped("remap_column_stores");
}

#[test]
fn a_failed_type_index_rebase_pins_the_generation_the_index_still_maps() {
    a_failed_rebase_pins_the_generation_it_leaves_mapped("rebase_type_indices");
}

#[cfg(unix)]
#[test]
fn a_generation_that_cannot_be_deleted_defers_and_never_fails_the_save() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let doomed = generation_dir(path, *generation_ids(path).last().unwrap());
    // Entries cannot be unlinked from a directory the owner cannot write.
    let locked = doomed.join("seg_000");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe_delete = std::fs::remove_file(locked.join("columns_meta.json"));
    if probe_delete.is_ok() {
        // Running as a user the permission bits do not bind (root): nothing to test.
        return;
    }

    save_more(&mut graph, path, 3);
    assert!(
        doomed.exists(),
        "an undeletable generation stays until something can remove it"
    );
    // Back to writable: the next save removes it.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    save_more(&mut graph, path, 1);
    assert!(!doomed.exists());
    let report = prune_generations(Path::new(path), Some(1));
    assert_eq!(report, PruneReport::default(), "nothing left to do");
}

#[test]
fn a_legacy_flat_directory_and_a_fresh_one_have_nothing_to_prune() {
    let dir = TempDir::new().unwrap();
    assert_eq!(
        prune_generations(dir.path(), Some(0)),
        PruneReport::default()
    );
    let root = dir.path().join("no_current");
    std::fs::create_dir_all(root.join("generations/gen_00000000000000000001")).unwrap();
    assert_eq!(prune_generations(&root, Some(0)), PruneReport::default());
    assert!(root.join("generations/gen_00000000000000000001").exists());
}
