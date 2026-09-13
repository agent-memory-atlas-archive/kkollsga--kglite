use super::*;
use mcp_methods::server::{workspace, Manifest};

fn manifest_with(dir: &std::path::Path, body: &str) -> Manifest {
    let path = dir.join("mcp-manifest.yaml");
    std::fs::write(&path, body).expect("write manifest");
    mcp_methods::server::load_manifest(&path).expect("manifest loads")
}

fn open(dir: &std::path::Path, manifest: Option<&Manifest>) -> workspace::Workspace {
    let state = GraphState::new(Some(WorkspaceGraphMode::LocalWorkspace));
    local_workspace(dir.to_path_buf(), &state, manifest).expect("local workspace opens")
}

#[test]
fn relative_workspace_boundaries_resolve_from_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = tmp.path().join("config");
    let sandbox = tmp.path().join("sandbox");
    let child = sandbox.join("child");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&config).expect("mkdir config");
    std::fs::create_dir_all(&child).expect("mkdir child");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    let manifest = manifest_with(
        &config,
        "workspace:\n  kind: local\n  root: ../sandbox/child\n  sandbox_root: ../sandbox\n",
    );
    let mode = promote_local_workspace(Mode::Bare, Some(&manifest)).expect("promote mode");
    let Mode::LocalWorkspace { root, .. } = mode else {
        panic!("local mode")
    };
    assert_eq!(root, child.canonicalize().expect("canonical child"));
    let bounded = open(&root, Some(&manifest));
    let before = bounded.active_repo_path().expect("active root");
    let _ = bounded.set_root_dir(&outside, None);
    assert_eq!(
        bounded.active_repo_path().expect("active root remains"),
        before
    );
}

#[test]
fn absolute_parent_and_invalid_boundaries_keep_their_meaning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = tmp.path().join("config");
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&config).expect("mkdir config");
    std::fs::create_dir_all(&root).expect("mkdir root");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    for sandbox in [root.to_string_lossy().into_owned(), "../root".to_string()] {
        let manifest = manifest_with(
            &config,
            &format!(
                "workspace:\n  kind: local\n  root: ../root\n  sandbox_root: {:?}\n",
                sandbox
            ),
        );
        assert_eq!(
            open(&root, Some(&manifest))
                .active_repo_path()
                .expect("root"),
            root.canonicalize().expect("canonical root")
        );
    }
    for sandbox in ["missing", "../outside"] {
        let manifest = manifest_with(
            &config,
            &format!("workspace:\n  kind: local\n  root: ../root\n  sandbox_root: {sandbox}\n"),
        );
        let state = GraphState::new(Some(WorkspaceGraphMode::LocalWorkspace));
        let error = local_workspace(root.clone(), &state, Some(&manifest))
            .err()
            .expect("invalid boundary fails");
        assert!(error
            .to_string()
            .contains("workspace.sandbox_root is not usable"));
    }
}

#[cfg(unix)]
#[test]
fn boundary_symlink_and_addressed_manifest_symlink_keep_their_semantics() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    let launcher = tmp.path().join("launcher");
    std::fs::create_dir_all(&fixture).expect("mkdir fixture");
    std::fs::create_dir_all(&launcher).expect("mkdir launcher");
    symlink(".", fixture.join("sandbox-link")).expect("sandbox symlink");
    let target = fixture.join("target.yaml");
    std::fs::write(
        &target,
        "workspace:\n  kind: local\n  root: .\n  sandbox_root: sandbox-link\n",
    )
    .expect("write manifest");
    let direct = mcp_methods::server::load_manifest(&target).expect("direct manifest");
    let Mode::LocalWorkspace { root, .. } =
        promote_local_workspace(Mode::Bare, Some(&direct)).expect("direct mode")
    else {
        panic!("local mode")
    };
    open(&root, Some(&direct));
    std::fs::write(
        &target,
        "workspace:\n  kind: local\n  root: .\n  sandbox_root: .\n",
    )
    .expect("rewrite linked manifest control");
    let addressed = launcher.join("manifest-link.yaml");
    symlink(&target, &addressed).expect("manifest symlink");
    let linked = mcp_methods::server::load_manifest(&addressed).expect("linked manifest");
    let Mode::LocalWorkspace { root, .. } =
        promote_local_workspace(Mode::Bare, Some(&linked)).expect("linked mode")
    else {
        panic!("local mode")
    };
    assert_eq!(root, launcher.canonicalize().expect("canonical launcher"));
    open(&root, Some(&linked));
}

#[cfg(unix)]
#[test]
fn escaping_swap_symlink_is_rejected_without_changing_active_root() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().expect("tempdir");
    let sandbox = tmp.path().join("sandbox");
    let root = sandbox.join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).expect("mkdir root");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    symlink(&outside, sandbox.join("escape")).expect("escape symlink");
    let manifest = manifest_with(
        tmp.path(),
        "workspace:\n  kind: local\n  root: sandbox/root\n  sandbox_root: sandbox\n",
    );
    let bounded = open(&root, Some(&manifest));
    let before = bounded.active_repo_path().expect("active root");
    let _ = bounded.set_root_dir(&sandbox.join("escape"), None);
    assert_eq!(
        bounded.active_repo_path().expect("active root remains"),
        before
    );
}
