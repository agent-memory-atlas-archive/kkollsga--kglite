//! What a load does with the compressed sections it consumed.

use super::index_sections::with_floors;
use super::index_sections_tests::{add_unique_id_type, load, run, save};
use super::mappable::SaveLayout;
use super::*;

/// A graph whose compressed topology section is over the release floor: random
/// edges over many nodes do not compress away.
fn saved_with_a_large_compressed_section(
    layout: SaveLayout,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("large.kgl");
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Doc", 0..120_000);
    run(
        &mut graph,
        "UNWIND range(0, 119999) AS i MATCH (a:Doc {id: i}), (b:Doc {id: (i * 7919 + 13) % 120000}) \
         CREATE (a)-[:L]->(b)",
    );
    run(
        &mut graph,
        "UNWIND range(0, 119999) AS i MATCH (a:Doc {id: i}), (b:Doc {id: (i * 104729 + 7) % 120000}) \
         CREATE (a)-[:M]->(b)",
    );
    with_floors(10, 10, || save(graph, &path, layout));
    (tmp, path)
}

#[test]
fn a_load_releases_the_compressed_sections_it_consumed() {
    let (_tmp, path) = saved_with_a_large_compressed_section(SaveLayout::Compact);
    let file_len = std::fs::metadata(&path).unwrap().len() as usize;
    for storage in [StorageMode::Memory, StorageMode::Mapped] {
        super::section_cursor::RELEASED.with(|released| released.borrow_mut().clear());
        let graph = load(&path, storage);
        let released = super::section_cursor::RELEASED.with(|released| released.borrow().clone());
        assert!(
            released
                .iter()
                .any(|(offset, len)| *len >= 1 << 20 && offset + len <= file_len),
            "{storage:?}: a payload of a megabyte or more is released: {released:?}"
        );
        assert!(
            released.iter().all(|(_, len)| *len >= 1 << 20),
            "{storage:?}: a small section costs a syscall and holds no pages worth freeing"
        );
        assert_eq!(graph.graph.node_count(), 120_000);
    }
}
