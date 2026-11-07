//! Workspace snapshots and forwarding rules, as they are written and read back.
//!
//! A restart is a second store on the same directory. Restore is then asked for
//! pending panes only. These tests fail if that step runs a ProxyCommand, starts
//! a shell, or binds a forwarded port, and they fail if a damaged or newer file
//! is treated as an empty workspace.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shellune_core::{
    Endpoint, ForwardBook, ForwardKind, ForwardRule, HostConfig, HostId, PaneNode, PaneTarget,
    PanelConfig, PanelSection, PendingConnection, SplitAxis, TerminalMode, WorkspaceSnapshot,
    WorkspaceTab,
};
use shellune_storage::{
    CONFIG_KEYS, FORWARD_FILE_NAME, ForwardStore, PersistError, WORKSPACE_FILE_NAME, WorkspaceStore,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "shellune-workspace-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            tag
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory can be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn listen(port: u16) -> Endpoint {
    Endpoint::loopback(port).expect("loopback port")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().expect("the port is known").port();
    drop(listener);
    port
}

fn side_effect_script(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("restore-ran.txt");
    let script = if cfg!(windows) {
        let script = dir.join("should-not-run.cmd");
        std::fs::write(
            &script,
            format!("@echo off\r\necho leaked>\"{}\"\r\n", marker.display()),
        )
        .expect("the script can be written");
        script
    } else {
        let script = dir.join("should-not-run.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho leaked > \"{}\"\n", marker.display()),
        )
        .expect("the script can be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script)
                .expect("the script exists")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions)
                .expect("the script can be marked executable");
        }
        script
    };
    (script, marker)
}

fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                keys.push(key.clone());
                collect_keys(value, keys);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_keys(item, keys);
            }
        }
        _ => {}
    }
}

fn assert_allowlisted(bytes: &str) {
    let document: serde_json::Value = serde_json::from_str(bytes).expect("json");
    let mut keys = Vec::new();
    collect_keys(&document, &mut keys);
    for key in keys {
        assert!(
            CONFIG_KEYS.contains(&key.as_str()),
            "the saved document uses the key {key:?}, which is not on the allowlist"
        );
        let lower = key.to_lowercase();
        for forbidden in [
            "password",
            "passphrase",
            "secret",
            "token",
            "credential",
            "private",
        ] {
            assert!(
                !lower.contains(forbidden),
                "the saved document has a key that names a secret: {key:?}"
            );
        }
    }
}

#[test]
fn a_snapshot_round_trips_tabs_splits_and_the_panel() {
    let dir = TempDir::new("round-trip");
    let empty = WorkspaceStore::at(dir.path())
        .load()
        .expect("a missing snapshot is an empty workspace");
    assert!(empty.tabs().is_empty());
    assert!(
        ForwardStore::at(dir.path())
            .load()
            .expect("a missing rule file is an empty book")
            .rules()
            .is_empty()
    );
    let host = HostId::new();
    let local = PaneNode::leaf(
        PaneTarget::local(Some("pwsh".to_owned())),
        TerminalMode::Classic,
    );
    let remote = PaneNode::leaf(PaneTarget::remote(host), TerminalMode::Integrated);
    let nested = PaneNode::split(SplitAxis::TopBottom, 2500, remote, local).expect("ratio");
    let root = PaneNode::split(
        SplitAxis::LeftRight,
        6000,
        PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
        nested,
    )
    .expect("ratio");
    let mut servers = WorkspaceTab::new("Servers", root).expect("title");
    let active = match servers.root() {
        PaneNode::Split { first, .. } => match first.as_ref() {
            PaneNode::Leaf { pane } => pane.id(),
            PaneNode::Split { .. } => panic!("the first child is a leaf"),
        },
        PaneNode::Leaf { .. } => panic!("the root is a split"),
    };
    servers.set_active_pane(Some(active));
    let local_tab = WorkspaceTab::new(
        "Local",
        PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
    )
    .expect("title");
    let mut snapshot = WorkspaceSnapshot::new(
        vec![servers, local_tab],
        PanelConfig::new(true, PanelSection::Forwards),
    )
    .expect("snapshot");
    let active_tab = snapshot.tabs()[0].id();
    snapshot.set_active_tab(Some(active_tab));

    let store = WorkspaceStore::at(dir.path());
    store.save(&snapshot).expect("saves");
    let loaded = WorkspaceStore::at(dir.path()).load().expect("loads");
    assert_eq!(loaded, snapshot);
    assert_eq!(loaded.panel().section(), PanelSection::Forwards);
    assert!(loaded.panel().visible());
    assert_eq!(loaded.tabs().len(), 2);
    assert_eq!(loaded.tabs()[0].title(), "Servers");
    assert_eq!(loaded.active_tab(), Some(active_tab));
    assert_allowlisted(&std::fs::read_to_string(store.path()).expect("file"));
}

#[test]
fn restore_after_a_restart_is_pending_and_starts_nothing() {
    let dir = TempDir::new("restore");
    let (script, marker) = side_effect_script(dir.path());
    let secret = "hunter2-restore";
    let port = free_port();
    let saved_host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("host");
    let gone = HostId::new();

    let mut host = saved_host.clone();
    host.set_proxy_command(Some(script.display().to_string()));
    host.set_startup_command(Some(secret));

    let root = PaneNode::split(
        SplitAxis::LeftRight,
        5000,
        PaneNode::leaf(
            PaneTarget::local(Some(script.display().to_string())),
            TerminalMode::Classic,
        ),
        PaneNode::split(
            SplitAxis::TopBottom,
            4000,
            PaneNode::leaf(
                PaneTarget::remote(saved_host.id()),
                TerminalMode::Integrated,
            ),
            PaneNode::leaf(PaneTarget::remote(gone), TerminalMode::Classic),
        )
        .expect("inner"),
    )
    .expect("outer");
    let mut tab = WorkspaceTab::new("Work", root).expect("title");
    let stale_pane = shellune_core::PaneId::new();
    tab.set_active_pane(Some(stale_pane));
    let mut snapshot =
        WorkspaceSnapshot::new(vec![tab], PanelConfig::new(true, PanelSection::Files))
            .expect("snapshot");
    let stale_tab = shellune_core::TabId::new();
    snapshot.set_active_tab(Some(stale_tab));

    let mut rules = ForwardBook::new();
    let local_rule = ForwardRule::local(
        saved_host.id(),
        listen(port),
        Endpoint::new("db.internal", 5432).expect("target"),
    );
    let dynamic = ForwardRule::dynamic(gone, listen(port.saturating_add(1).max(1)));
    let remote = ForwardRule::remote(
        saved_host.id(),
        Endpoint::new("0.0.0.0", 9000).expect("remote listen"),
        listen(80),
    );
    rules.insert(local_rule.clone()).expect("local");
    rules.insert(dynamic.clone()).expect("dynamic");
    rules.insert(remote.clone()).expect("remote");

    WorkspaceStore::at(dir.path())
        .save(&snapshot)
        .expect("snapshot saves");
    ForwardStore::at(dir.path())
        .save(&rules)
        .expect("rules save");
    assert!(
        !marker.exists(),
        "saving the snapshot must not run the shell or the ProxyCommand"
    );
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("saving a rule must not listen");

    let snapshot = WorkspaceStore::at(dir.path())
        .load()
        .expect("snapshot loads");
    let rules = ForwardStore::at(dir.path()).load().expect("rules load");
    assert_eq!(rules.rules().len(), 3);
    assert_eq!(
        rules.rule(local_rule.id()).expect("local").kind(),
        ForwardKind::Local
    );
    assert_eq!(rules.rule(dynamic.id()).expect("dynamic").target(), None);
    assert!(rules.rule(remote.id()).expect("remote").target().is_some());
    assert!(!local_rule.needs_exposure_confirmation());
    assert!(!remote.needs_exposure_confirmation());

    let restored = snapshot.restore(|id| id == saved_host.id());
    assert_eq!(restored.tabs().len(), 1);
    assert_eq!(restored.panel().section(), PanelSection::Files);
    assert!(restored.active_tab().is_none());
    let panes = restored.panes();
    assert_eq!(panes.len(), 3);
    assert!(panes.iter().all(|pane| !pane.connection().is_started()));
    assert!(panes.iter().any(|pane| matches!(
        pane.connection(),
        PendingConnection::Remote { host } if *host == saved_host.id()
    )));
    assert!(
        restored
            .issues()
            .iter()
            .any(|issue| issue.message().contains(&gone.to_string())
                && issue.message().contains("Nothing was connected")),
        "the missing host is local: {:?}",
        restored.issues()
    );
    assert!(restored.issues().iter().any(|issue| {
        issue.message().contains(&stale_tab.to_string()) && issue.message().contains("Choose")
    }));
    assert!(
        restored
            .issues()
            .iter()
            .any(|issue| { issue.message().contains(&stale_pane.to_string()) })
    );
    let forwarded = rules.faults(|id| id == saved_host.id());
    assert_eq!(forwarded.len(), 1);
    assert!(forwarded[0].message().contains("not started"));
    assert!(
        rules.rule(local_rule.id()).is_some(),
        "the good rule is kept"
    );

    assert!(!marker.exists(), "restore must not run a process");
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("restore must not listen");
    let rendered = format!("{restored:?}{rules:?}{host:?}");
    assert!(
        !std::fs::read_to_string(dir.path().join(WORKSPACE_FILE_NAME))
            .expect("snapshot file")
            .contains(secret),
        "a startup command is not part of the snapshot"
    );
    assert!(!rendered.contains("password"));
    assert_allowlisted(
        &std::fs::read_to_string(dir.path().join(FORWARD_FILE_NAME)).expect("rules file"),
    );
}

#[test]
fn a_newer_or_damaged_snapshot_is_an_error_and_the_file_stays() {
    let dir = TempDir::new("snapshot-damage");
    let store = WorkspaceStore::at(dir.path());
    let path = store.path();
    let secret = "hunter2-snapshot";

    std::fs::write(&path, "{ this is not a snapshot").expect("written");
    let before = std::fs::read_to_string(&path).expect("read");
    let error = store.load().expect_err("damaged");
    assert!(matches!(error, PersistError::Corrupt { .. }), "{error:?}");
    assert!(error.to_string().contains("workspace.json"), "{error}");
    assert!(error.to_string().len() > 40, "{error}");
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), before);

    let newer = format!(
        r#"{{"version":7,"tabs":[],"panel":{{"visible":false,"section":"none"}},"password":"{secret}"}}"#
    );
    std::fs::write(&path, &newer).expect("written");
    let error = store.load().expect_err("newer");
    assert!(
        matches!(
            error,
            PersistError::UnsupportedVersion {
                found: 7,
                supported: 1,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("left unchanged"), "{error}");
    assert!(!error.to_string().contains(secret), "{error}");
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), newer);

    let poisoned = format!(
        r#"{{"version":1,"tabs":[],"panel":{{"visible":false,"section":"none"}},"password":"{secret}"}}"#
    );
    std::fs::write(&path, &poisoned).expect("written");
    let error = store.load().expect_err("secret field");
    assert!(matches!(error, PersistError::Corrupt { .. }), "{error:?}");
    assert!(error.to_string().contains("password"), "{error}");
    assert!(!error.to_string().contains(secret), "{error}");
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), poisoned);
}

#[test]
fn a_duplicate_pane_is_refused_without_becoming_an_empty_workspace() {
    let dir = TempDir::new("duplicate-pane");
    let pane = PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic);
    let id = match pane {
        PaneNode::Leaf { ref pane } => pane.id(),
        PaneNode::Split { .. } => panic!("leaf"),
    };
    let tab = WorkspaceTab::new("One", pane).expect("tab");
    let snapshot = WorkspaceSnapshot::new(vec![tab], PanelConfig::default()).expect("snapshot");
    let mut value = serde_json::to_value(&snapshot).expect("value");
    let pane = value["tabs"][0]["root"].clone();
    value["tabs"][0]["root"] = serde_json::json!({
        "node": "split",
        "axis": "left_right",
        "ratio": 5000,
        "first": pane,
        "second": value["tabs"][0]["root"].clone(),
    });
    // Both children are the same leaf, so the pane id repeats.
    let _ = id;
    let store = WorkspaceStore::at(dir.path());
    std::fs::create_dir_all(dir.path()).expect("dir");
    std::fs::write(store.path(), serde_json::to_string(&value).expect("json")).expect("written");
    let error = store.load().expect_err("duplicate pane");
    assert!(matches!(error, PersistError::Corrupt { .. }), "{error:?}");
    assert!(
        error.to_string().contains("two panes share the id"),
        "{error}"
    );
    assert!(
        error.to_string().contains("Nothing was connected"),
        "{error}"
    );
}

#[test]
fn forwarding_damage_and_a_missing_host_stay_specific() {
    let dir = TempDir::new("forward-damage");
    let store = ForwardStore::at(dir.path());
    let path = store.path();
    let secret = "hunter2-forward";

    std::fs::write(&path, "not-json").expect("written");
    let error = store.load().expect_err("damaged");
    assert!(matches!(error, PersistError::Corrupt { .. }), "{error:?}");
    assert!(error.to_string().contains("forwards.json"), "{error}");
    assert!(!error.to_string().contains(secret));

    let newer = format!(r#"{{"version":4,"rules":[],"password":"{secret}"}}"#);
    std::fs::write(&path, &newer).expect("written");
    let error = store.load().expect_err("newer");
    assert!(
        matches!(error, PersistError::UnsupportedVersion { found: 4, .. }),
        "{error:?}"
    );
    assert!(!error.to_string().contains(secret), "{error}");
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), newer);

    let id = shellune_core::ForwardId::new();
    let host = HostId::new();
    let dynamic = format!(
        r#"{{"version":1,"rules":[{{"id":"{id}","host":"{host}","kind":"dynamic","listen":{{"host":"127.0.0.1","port":1080}},"target":{{"host":"db","port":1}}}}]}}"#
    );
    std::fs::write(&path, &dynamic).expect("written");
    let error = store.load().expect_err("dynamic target");
    assert!(error.to_string().contains("remove the target"), "{error}");
    assert!(error.to_string().contains(&id.to_string()), "{error}");

    let local = ForwardRule::local(
        host,
        Endpoint::new("0.0.0.0", 8080).expect("exposed"),
        Endpoint::new("db.internal", 5432).expect("target"),
    );
    assert!(local.needs_exposure_confirmation());
    let mut book = ForwardBook::new();
    book.insert(local).expect("insert");
    store.save(&book).expect("saved");
    let loaded = ForwardStore::at(dir.path()).load().expect("loads");
    let faults = loaded.faults(|_| false);
    assert_eq!(faults.len(), 1);
    assert!(faults[0].message().contains("Choose a saved host"));
    assert!(!faults[0].message().contains(secret));
    assert_eq!(loaded.rules().len(), 1, "the rule is kept");
}
