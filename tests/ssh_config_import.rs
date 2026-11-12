//! Importing a real SSH configuration file (R5, R14-R16).
//!
//! These tests use the file system rather than an in-memory string where it matters:
//! the source file is written by the test, imported, and compared byte for byte
//! afterwards, and the imported records go through the same store the application
//! writes. What they do not do is claim more than the importer promises: every
//! limitation is asserted as a limitation, and a blocked host is asserted to be
//! absent from the saved book rather than merely warned about.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shellune_storage::{HostStore, ImportError, ImportOutcome, ImportPreview, NoteKind};

/// A directory that removes itself, so a failing assertion leaves no state behind.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "shellune-import-{}-{}-{}",
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

    fn config(&self) -> PathBuf {
        self.path.join("config")
    }

    fn write_config(&self, text: &str) -> PathBuf {
        std::fs::write(self.config(), text).expect("the test writes the configuration");
        self.config()
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.path)
            .expect("the directory is readable")
            .map(|entry| {
                entry
                    .expect("a readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn store(&self) -> HostStore {
        HostStore::at(self.path.join("data"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

const CONFIG: &str = "\
# The configuration this user already had
Host web
    HostName web.example.org
    User deploy
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host db
    HostName 10.0.0.6
    User postgres
    ServerAliveInterval 30

Host through-bastion
    HostName internal.example.org
    User root
    ProxyJump bastion.example.org
";

#[test]
fn imported_hosts_reach_the_book_and_the_source_file_is_never_written() {
    let dir = TempDir::new("apply");
    let config = dir.write_config(CONFIG);
    let before = std::fs::read(&config).expect("the configuration is readable");
    let modified = std::fs::metadata(&config)
        .expect("the configuration has metadata")
        .modified()
        .expect("the platform reports a modification time");

    let preview = ImportPreview::from_file(&config).expect("the configuration parses");
    assert_eq!(
        preview.source(),
        config.as_path(),
        "the preview keeps the path it read"
    );
    assert_eq!(preview.hosts().len(), 3);
    assert_eq!(
        preview.importable_count(),
        2,
        "the proxied host is not offered"
    );
    assert_eq!(preview.blocked_count(), 1);

    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    let plan = preview.plan(&book);
    assert_eq!(plan.len(), 2);
    plan.apply(&mut book).expect("the hosts are added");
    store.save(&book).expect("the book saves");

    // The next launch reads what this one wrote.
    let restarted = HostStore::at(dir.path().join("data"))
        .load()
        .expect("the file loads");
    assert_eq!(restarted.hosts().len(), 2);
    let web = restarted
        .hosts()
        .iter()
        .find(|host| host.name() == "web")
        .expect("the imported host is there");
    assert_eq!(web.target(), "deploy@web.example.org:2222");
    assert_eq!(web.identity_file(), Some("~/.ssh/id_ed25519"));
    assert_eq!(web.group(), None, "the importer does not invent a group");
    assert!(
        restarted
            .hosts()
            .iter()
            .all(|host| host.name() != "through-bastion"),
        "a host that needs a jump host is not saved as a direct connection"
    );

    // The file ssh itself reads is exactly as it was, down to its modification time.
    let after = std::fs::read(&config).expect("the configuration is still readable");
    assert_eq!(after, before, "the import must not change the source file");
    assert_eq!(
        std::fs::metadata(&config)
            .expect("the configuration has metadata")
            .modified()
            .expect("the platform reports a modification time"),
        modified,
        "the import must not even rewrite the source file"
    );
    assert_eq!(
        dir.entries(),
        vec!["config".to_owned(), "data".to_owned()],
        "the import leaves no temporary file next to the configuration"
    );
}

#[test]
fn an_imported_host_is_edited_in_the_application_like_any_other_record() {
    let dir = TempDir::new("edit");
    let config = dir.write_config(CONFIG);
    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    let preview = ImportPreview::from_file(&config).expect("the configuration parses");
    preview
        .plan(&book)
        .apply(&mut book)
        .expect("the hosts are added");
    store.save(&book).expect("the book saves");

    let id = book
        .hosts()
        .iter()
        .find(|host| host.name() == "web")
        .expect("the host was imported")
        .id();
    let mut edited = book.host(id).expect("the record exists").clone();
    edited
        .set_address("web2.example.org")
        .expect("a valid address");
    edited.set_user("ops").expect("a valid user");
    edited.set_port(2200).expect("a valid port");
    edited.set_identity_file(Some("C:/keys/web2_ed25519"));
    book.update(edited, id).expect("the edit applies");
    store.save(&book).expect("the edited book saves");

    let restarted = HostStore::at(dir.path().join("data"))
        .load()
        .expect("the file loads");
    let host = restarted.host(id).expect("the same record");
    assert_eq!(host.target(), "ops@web2.example.org:2200");
    assert_eq!(host.identity_file(), Some("C:/keys/web2_ed25519"));

    // The edit is an application record; the configuration keeps its own values.
    let text = std::fs::read_to_string(&config).expect("the configuration is readable");
    assert!(text.contains("web.example.org"));
    assert!(!text.contains("web2.example.org"));
}

#[test]
fn a_malformed_configuration_reports_its_line_and_changes_nothing() {
    let dir = TempDir::new("syntax");
    let config = dir.write_config("Host web\n    HostName web.example.org\n    Port\n");
    let store = dir.store();

    let error = ImportPreview::from_file(&config).expect_err("the file is malformed");
    match &error {
        ImportError::Syntax { path, line, reason } => {
            assert_eq!(path, &config, "the failure names the file that was read");
            assert_eq!(*line, 3, "the failure names the line");
            assert!(
                reason.contains("Port"),
                "the failure names the statement: {reason}"
            );
        }
        other => panic!("a malformed file must report its line, got {other:?}"),
    }
    assert!(error.to_string().contains("line 3"), "{error}");
    assert!(
        !store.path().exists(),
        "a failed import must not create the host file"
    );
}

#[test]
fn a_missing_file_reports_the_path_and_nothing_is_imported() {
    let dir = TempDir::new("missing");
    let missing = dir.path().join("no-such-config");
    let error = ImportPreview::from_file(&missing).expect_err("the file does not exist");
    assert_eq!(error.path(), missing.as_path());
    assert!(error.to_string().contains("no-such-config"), "{error}");
    assert_eq!(error.line(), None);
}

#[test]
fn an_include_is_reported_before_anything_is_applied_but_the_plain_hosts_still_import() {
    let dir = TempDir::new("include");
    let config = dir.write_config(
        "\
Include config.d/*

Host web
    HostName web.example.org
    User deploy

Match host db
    User dbuser
",
    );

    let preview = ImportPreview::from_file(&config).expect("the configuration parses");
    assert!(
        preview.is_partial(),
        "an Include cannot be a complete import"
    );
    assert_eq!(preview.limits().include_lines(), &[1]);
    assert_eq!(preview.limits().match_lines(), &[7]);

    let web = preview
        .hosts()
        .iter()
        .find(|host| host.alias() == "web")
        .expect("the plain host is listed");
    assert_eq!(web.outcome(), ImportOutcome::NeedsReview);
    assert!(
        web.is_importable(),
        "an Include is a limitation, not a reason to drop a plain host"
    );
    assert!(
        web.notes()
            .iter()
            .any(|note| note.reason().contains("does not read")),
        "the preview says which files were not read: {:?}",
        web.notes()
    );

    // A proxy inside a Match block could apply to the plain host, so nothing is
    // offered at all and the saved book stays as it was.
    let store = dir.store();
    let book = store.load().expect("a first run is not a failure");
    let preview = ImportPreview::from_file(dir.write_config(
        "\
Host web
    HostName web.example.org
    User deploy

Match exec \"true\"
    ProxyJump bastion
",
    ))
    .expect("the configuration parses");
    let plan = preview.plan(&book);
    assert!(plan.is_empty(), "a conditional proxy blocks the import");
    assert_eq!(book.hosts().len(), 0);
    assert!(!store.path().exists());
}

#[test]
fn a_proxy_on_a_pattern_no_host_matches_is_reported_rather_than_dropped() {
    // ssh would still apply these rules to a name this file never defines, so the
    // preview cannot call the file completely read. Both the wildcard form and the
    // negated form are the same omission and are named with their line.
    let dir = TempDir::new("unmatched-proxy");
    let config = dir.write_config(
        "\
Host *.corp
    ProxyJump bastion corp

Host app
    HostName app.example.org
    User deploy

Host !bad web*
    ProxyCommand nc %h %p
",
    );
    let before = std::fs::read(&config).expect("the configuration is readable");

    let preview = ImportPreview::from_file(&config).expect("the configuration parses");
    let named: Vec<(usize, &str)> = preview
        .limits()
        .unmatched_proxy()
        .iter()
        .map(|note| (note.line(), note.keyword()))
        .collect();
    assert_eq!(
        named,
        vec![(1, "*.corp"), (8, "!bad web*")],
        "each skipped pattern is reported with its Host line: {:?}",
        preview.limits()
    );
    assert!(
        preview.is_partial(),
        "a proxy the preview could not place means the file was not read completely"
    );
    assert!(
        preview
            .limits()
            .unmatched_proxy()
            .iter()
            .all(|note| note.reason().contains("Proxy")),
        "the reason names the directive that was skipped: {:?}",
        preview.limits().unmatched_proxy()
    );
    assert_eq!(
        preview.importable_count(),
        1,
        "the plain host is still offered; the skipped rules do not block it"
    );

    // Importing writes the offered host, leaves the source file byte for byte as it
    // was, and stores no rule the preview said it had skipped.
    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    preview
        .plan(&book)
        .apply(&mut book)
        .expect("the offered host is added");
    store.save(&book).expect("the book saves");
    assert_eq!(book.hosts().len(), 1);
    assert_eq!(book.hosts()[0].name(), "app");
    assert_eq!(
        std::fs::read(&config).expect("the configuration is still readable"),
        before,
        "the import must not change the source file"
    );
}

#[test]
fn importing_the_same_file_twice_does_not_duplicate_the_saved_hosts() {
    let dir = TempDir::new("twice");
    let config = dir.write_config(CONFIG);
    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");

    let first = ImportPreview::from_file(&config).expect("the configuration parses");
    first
        .plan(&book)
        .apply(&mut book)
        .expect("the hosts are added");
    store.save(&book).expect("the book saves");

    let second = ImportPreview::from_file(&config).expect("the configuration parses again");
    let plan = second.plan(&book);
    assert!(plan.is_empty(), "a second import would duplicate nothing");
    assert_eq!(
        plan.skipped().len(),
        2,
        "both hosts are named as the reason they were skipped"
    );
    assert!(
        plan.skipped()
            .iter()
            .all(|skipped| skipped.reason().contains("already named"))
    );
    let mut unchanged = book.clone();
    plan.apply(&mut unchanged).expect("an empty plan applies");
    assert_eq!(unchanged.hosts().len(), 2);
}

#[test]
fn a_complete_jump_proxy_command_and_forwards_are_saved_without_touching_the_source() {
    let dir = TempDir::new("chain-forward");
    let config = dir.write_config(
        "\
Host bastion
    HostName bastion.example.org
    User jump

Host web
    HostName web.example.org
    User deploy
    ProxyJump bastion

Host files
    HostName files.example.org
    User deploy
    ProxyCommand ssh -W %h:%p bastion
    LocalForward 8080 localhost:80
    RemoteForward 9090 db.internal:5432
    DynamicForward 1080
",
    );
    let before = std::fs::read(&config).expect("the configuration is readable");
    let preview = ImportPreview::from_file(&config).expect("the configuration parses");
    let web = preview
        .hosts()
        .iter()
        .find(|host| host.alias() == "web")
        .expect("web is in the preview");
    assert!(web.is_importable(), "a complete ProxyJump is offered");
    assert_eq!(web.jumps()[0].alias(), Some("bastion"));
    let files = preview
        .hosts()
        .iter()
        .find(|host| host.alias() == "files")
        .expect("files is in the preview");
    assert_eq!(files.proxy_command(), Some("ssh -W %h:%p bastion"));
    assert_eq!(
        files.forwards().len(),
        3,
        "three complete forwards stay visible"
    );

    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    let plan = preview.plan(&book);
    assert_eq!(plan.forwards().len(), 3);
    plan.apply(&mut book).expect("the hosts are added");
    store.save(&book).expect("the book saves");

    let bastion = book
        .hosts()
        .iter()
        .find(|host| host.name() == "bastion")
        .expect("the jump host is saved");
    let web = book
        .hosts()
        .iter()
        .find(|host| host.name() == "web")
        .expect("the jumped host is saved");
    assert_eq!(web.jumps(), &[bastion.id()]);
    assert!(web.proxy_command().is_none());
    let files_host = book
        .hosts()
        .iter()
        .find(|host| host.name() == "files")
        .expect("the proxied host is saved");
    assert_eq!(files_host.proxy_command(), Some("ssh -W %h:%p bastion"));

    let forward_store = shellune_storage::ForwardStore::at(store.root());
    let mut forwards = forward_store
        .load()
        .expect("a missing forward file is empty");
    for planned in plan.forwards() {
        let host = book
            .hosts()
            .iter()
            .find(|host| host.name().eq_ignore_ascii_case(planned.alias()))
            .expect("the forward names a saved host");
        forwards
            .insert(planned.forward().rule(host.id()).expect("a complete rule"))
            .expect("the rule is stored");
    }
    forward_store
        .save(&forwards)
        .expect("the forwarding file is written");
    let loaded = shellune_storage::ForwardStore::at(store.root())
        .load()
        .expect("the forwarding file loads");
    assert_eq!(loaded.rules().len(), 3);
    assert!(loaded.rules().iter().any(|rule| {
        rule.kind() == shellune_core::ForwardKind::Local && rule.host() == files_host.id()
    }));
    assert!(
        loaded
            .rules()
            .iter()
            .any(|rule| rule.kind() == shellune_core::ForwardKind::Remote)
    );
    assert!(
        loaded
            .rules()
            .iter()
            .any(|rule| rule.kind() == shellune_core::ForwardKind::Dynamic)
    );
    assert_eq!(
        std::fs::read(&config).expect("the configuration is still readable"),
        before,
        "planning and saving do not run or rewrite the source"
    );
}

#[test]
fn x11_forwarding_from_the_file_reaches_the_saved_host_without_touching_the_source() {
    let dir = TempDir::new("x11");
    let config = dir.write_config(
        "\
Host restricted
    HostName r.example.org
    User deploy
    ForwardX11 yes

Host trusted
    HostName t.example.org
    User deploy
    ForwardX11Trusted yes

Host plain
    HostName p.example.org
    User deploy
",
    );
    let before = std::fs::read(&config).expect("the configuration is readable");
    let preview = ImportPreview::from_file(&config).expect("the configuration parses");

    assert_eq!(
        preview
            .hosts()
            .iter()
            .find(|host| host.alias() == "restricted")
            .expect("restricted is previewed")
            .x11(),
        Some(shellune_core::X11Forwarding::Restricted),
        "ForwardX11 yes is the restricted mode"
    );
    let trusted = preview
        .hosts()
        .iter()
        .find(|host| host.alias() == "trusted")
        .expect("trusted is previewed");
    assert_eq!(
        trusted.x11(),
        Some(shellune_core::X11Forwarding::Trusted),
        "ForwardX11Trusted yes is the trusted mode"
    );
    assert!(
        trusted.summary().contains("X11"),
        "the preview row says the host forwards a display: {}",
        trusted.summary()
    );
    assert_eq!(
        preview
            .hosts()
            .iter()
            .find(|host| host.alias() == "plain")
            .expect("plain is previewed")
            .x11(),
        None,
        "a host the file says nothing about forwards nothing"
    );

    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    let plan = preview.plan(&book);
    plan.apply(&mut book).expect("the hosts are added");
    store.save(&book).expect("the book saves");

    let saved = |name: &str| {
        book.hosts()
            .iter()
            .find(|host| host.name() == name)
            .expect("the host is saved")
            .x11()
    };
    assert_eq!(
        saved("restricted"),
        shellune_core::X11Forwarding::Restricted
    );
    assert_eq!(saved("trusted"), shellune_core::X11Forwarding::Trusted);
    assert_eq!(
        saved("plain"),
        shellune_core::X11Forwarding::Off,
        "a host saved from a file that says nothing forwards nothing"
    );
    assert_eq!(
        std::fs::read(&config).expect("the configuration is readable"),
        before,
        "the import never writes to the client's own configuration"
    );
}

#[test]
fn keepalive_directives_reach_the_saved_host_and_tcp_keepalive_stays_unapplied() {
    let dir = TempDir::new("keepalive");
    let config = dir.write_config(
        "\
Host probed
    HostName probed.example.org
    User deploy
    ServerAliveInterval 45
    ServerAliveCountMax 5
    TCPKeepAlive no

Host quiet
    HostName quiet.example.org
    User deploy
    ServerAliveInterval 0

Host broken
    HostName broken.example.org
    User deploy
    ServerAliveInterval 0seconds
",
    );
    let before = std::fs::read(&config).expect("the configuration is readable");
    let preview = ImportPreview::from_file(&config).expect("the configuration parses");

    let host = |alias: &str| {
        preview
            .hosts()
            .iter()
            .find(|host| host.alias() == alias)
            .unwrap_or_else(|| panic!("{alias} is previewed"))
    };

    // The two ServerAlive directives are applied, and the preview row says so.
    let probed = host("probed");
    let connection = probed
        .connection()
        .expect("the file's keepalive is in the preview");
    assert_eq!(connection.keepalive_interval_secs(), Some(45));
    assert_eq!(connection.keepalive_max_misses(), Some(5));
    assert!(
        probed.summary().contains("45"),
        "the preview row states the interval it will apply: {}",
        probed.summary()
    );

    // TCPKeepAlive is a different, forgeable mechanism and is not silently applied as
    // keepalive: the line is named, and the file is partial because of it.
    assert_eq!(
        probed.outcome(),
        ImportOutcome::NeedsReview,
        "a directive that is not applied is shown, not hidden"
    );
    let tcp = probed
        .notes()
        .iter()
        .find(|note| note.keyword() == "TCPKeepAlive")
        .expect("the unapplied line is named");
    assert_eq!(tcp.kind(), NoteKind::Unsupported);
    assert!(tcp.reason().contains("not applied"), "{}", tcp.reason());
    assert_eq!(
        tcp.line(),
        6,
        "the note points at the line the user can edit"
    );
    assert!(
        preview
            .limits()
            .ignored()
            .iter()
            .any(|note| note.keyword() == "TCPKeepAlive"),
        "the file-level list names it too: {:?}",
        preview.limits().ignored()
    );
    assert!(
        !preview
            .limits()
            .ignored()
            .iter()
            .any(|note| note.keyword() == "ServerAliveInterval"),
        "an applied directive is no longer reported as unsupported: {:?}",
        preview.limits().ignored()
    );

    // Interval 0 is OpenSSH's explicit disable and is kept as one.
    let quiet = host("quiet");
    let connection = quiet.connection().expect("the disable is kept");
    assert_eq!(connection.keepalive_interval_secs(), Some(0));
    assert!(
        quiet.summary().contains("keepalive off"),
        "{}",
        quiet.summary()
    );

    // A value that is not a number is refused with the line, and nothing is applied.
    let broken = host("broken");
    assert_eq!(broken.outcome(), ImportOutcome::NeedsReview);
    assert!(broken.connection().is_none(), "nothing was stored");
    let note = broken
        .notes()
        .iter()
        .find(|note| note.keyword() == "ServerAliveInterval")
        .expect("the refused line is named");
    assert_eq!(note.kind(), NoteKind::InvalidValue);
    assert!(note.reason().contains("whole number"), "{}", note.reason());

    // What the preview showed is what the store keeps, and it survives a restart.
    let store = dir.store();
    let mut book = store.load().expect("a first run is not a failure");
    preview
        .plan(&book)
        .apply(&mut book)
        .expect("the hosts are added");
    store.save(&book).expect("the book saves");

    let restarted = HostStore::at(dir.path().join("data"))
        .load()
        .expect("the file loads");
    let saved = |name: &str| {
        restarted
            .hosts()
            .iter()
            .find(|host| host.name() == name)
            .unwrap_or_else(|| panic!("{name} was saved"))
            .clone()
    };
    let probed = saved("probed");
    let connection = probed.connection().expect("the keepalive was saved");
    assert_eq!(connection.keepalive_interval_secs(), Some(45));
    assert_eq!(connection.keepalive_max_misses(), Some(5));

    // The saved choice is what a connection is dialled with, resolved against the
    // documented defaults, and the two independent directives stay independent.
    let defaults = shellune_core::ConnectionDefaults::default();
    let policy = probed.connection_policy(defaults);
    assert_eq!(
        policy.keepalive_interval(),
        Some(std::time::Duration::from_secs(45))
    );
    assert_eq!(policy.keepalive_max(), 5);
    assert_eq!(
        policy.connect_timeout(),
        std::time::Duration::from_secs(20),
        "the file said nothing about the deadline, so the default applies"
    );
    assert_eq!(
        policy.inactivity_timeout(),
        None,
        "an idle but answered session is not closed for silence"
    );

    let quiet = saved("quiet");
    assert!(
        quiet.connection_policy(defaults).keepalive().is_none(),
        "the file's explicit 0 is an off, not an inherited default"
    );

    assert_eq!(
        std::fs::read(&config).expect("the configuration is still readable"),
        before,
        "reading and saving never touches the client's own file"
    );
}
