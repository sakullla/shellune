//! The settings' "keys and passwords" page: the whole life of the managed SSH
//! keys and shared connection passwords the vault keeps (R1, ADR-7).
//!
//! Everything the page lists is public metadata - name, algorithm, fingerprint,
//! the connections that reference a record - because that is all a list, a log
//! line or a sync merge may ever see. The secrets live in the encrypted vault
//! and are read for one operation at a time: showing a password reveals it
//! until the page moves on, a private key export asks for its own one-time
//! passphrase and a second confirmation before it may be plain, and a
//! connection receives a one-time key body handed straight to the attempt.
//!
//! Every write is one background job: the vault is opened with this machine's
//! local unseal secret, the change is applied to the opened content as a whole,
//! and the vault is saved atomically - so an import, a rotation or a deletion
//! either lands complete or leaves the last good file exactly as it was. The
//! UI thread only ever sees the state without secrets and a notice that names
//! what happened; the window keeps drawing while the job runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, FontWeight, KeyDownEvent, ScrollHandle, SharedString, Stateful,
    Window, div, px,
};
use shellune_core::{
    ConnectionRecord, CredentialId, CredentialMeta, HostConfig, HostId, KeyId, ManagedKeyMeta,
    SecretString, VaultRecordVersion,
};
use shellune_ssh::keys as keys_api;
use shellune_storage::{
    AppSecret, BiometricAuthenticator, BiometricCapability, BiometricError, HostKey, HostReference,
    HostSecretState, KdfParams, LocalOnlyFallback, LockPolicy, PassphrasePurpose, SecretStore,
    SecuritySettings, SecurityStore, SystemAuthenticator, TrustEntry, UnsealSource, VaultContent,
    VaultCredentialRecord, VaultKeyRecord, VaultStore, VaultStoreError, VerificationPrompt,
};

use super::ShelluneView;
use super::shell::{ModalBehaviour, ModalContent};
use crate::app::components::{self, ControlState, Variant, checkbox};
use crate::app::copy::{self, Locale, SecurityStrings};
use crate::app::icons::Icon;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};
use crate::app::workspace::SectionStatus;

/// Minimum available width (before zoom) at which the home vault workbench uses a
/// side-by-side master-detail layout instead of a single column.
const VAULT_SPLIT_MIN_WIDTH: f32 = 760.0;

/// Width of the master-detail right column, before zoom.
const VAULT_DETAIL_WIDTH: f32 = 360.0;

/// Filter applied to the vault workbench lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SecurityFilter {
    /// Both SSH keys and shared passwords.
    #[default]
    All,
    /// SSH keys only.
    Keys,
    /// Shared passwords only.
    Credentials,
    /// Records not referenced by any saved connection.
    Unused,
}

impl SecurityFilter {
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Keys, Self::Credentials, Self::Unused];

    fn selector(self) -> &'static str {
        match self {
            Self::All => "security.filter.all",
            Self::Keys => "security.filter.keys",
            Self::Credentials => "security.filter.credentials",
            Self::Unused => "security.filter.unused",
        }
    }

    fn label(self, strings: &'static SecurityStrings) -> &'static str {
        match self {
            Self::All => strings.filter_all,
            Self::Keys => strings.filter_keys,
            Self::Credentials => strings.filter_credentials,
            Self::Unused => strings.filter_unused,
        }
    }
}

/// Which open connection draft should automatically bind a newly created vault record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingVaultBind {
    /// Bind the created/imported `KeyId` to the open SSH host draft.
    SshManagedKey,
    /// Bind the created `CredentialId` to the open SSH host draft.
    SshSharedCredential,
    /// Bind the created `CredentialId` to the open RDP/VNC connection draft.
    DesktopSharedCredential,
}

/// The widest the public key line of a detail card is drawn.
const PUBLIC_KEY_MAX_WIDTH: f32 = 520.0;

/// The least and the most characters a generated password may have.
const PASSWORD_LENGTH_MIN: usize = 8;
const PASSWORD_LENGTH_MAX: usize = 128;

/// The default length of a generated password, between the two bounds.
const PASSWORD_LENGTH_DEFAULT: usize = 20;

/// The floor below which an imported RSA key is refused outright (R1: a key
/// whose parameters are not supported must not be imported).
const RSA_IMPORT_FLOOR_BITS: usize = 2048;

/// The size this build itself generates. An imported RSA key between the floor
/// and this size is accepted with a warning: refusing it would block a key a
/// server still requires while saying nothing about why, and the warning is
/// what tells the reader a replacement is worth planning.
const RSA_GENERATED_BITS: usize = 3072;

/// Width of the security page's dialog, in logical pixels before the text scale.
const DIALOG_WIDTH: f32 = 560.0;

/// Width of the lock page's idle-minutes field, in logical pixels before the
/// text scale: a three-digit number and nothing more.
const IDLE_FIELD_WIDTH: f32 = 84.0;

// ---------------------------------------------------------------------------
// The page's state
// ---------------------------------------------------------------------------

/// The settings page of the vault: its lists, its one dialog, and the state of
/// the one background job that may be running.
pub(crate) struct SecurityPage {
    /// The search that narrows both lists.
    search: Entity<TextField>,
    /// The search that narrows the host-trust list of the security category's
    /// trust page (R7).
    trust_search: Entity<TextField>,
    /// The idle-lock minutes of the lock page, as they stand on this device.
    idle_field: Entity<TextField>,
    /// The vault as this window last read or wrote it, unlocked.
    ///
    /// `None` while no vault of this machine has been opened (or created) yet.
    /// Reads for one operation clone out of it; writes replace it with what the
    /// job saved. The lock of this device clears it (ADR-4): a locked vault
    /// holds no secret in memory, so every reader - the connection path, the
    /// page, a background sync - sees nothing until it is unlocked again.
    content: Option<VaultContent>,
    /// The device-local lock: its policy, its biometric setting and whether the
    /// vault is locked right now (R3, R4, ADR-4).
    lock: VaultLock,
    /// A verification already paid for lets one passage of the connection
    /// gate through: the resumed dial must not ask again for the pass it just
    /// got, and the next connection must (R3).
    pub(crate) operation_granted: bool,
    /// A vault job is running on the background executor.
    busy: bool,
    /// The vault is being opened at start-up; nothing may start meanwhile.
    warming: bool,
    /// Why the vault could not be opened at start-up, when it could not.
    open_problem: Option<String>,
    /// The notice of the last operation: its tone and its sentence, never a
    /// secret.
    notice: Option<(Tone, String)>,
    /// Filter applied to the vault lists.
    filter: SecurityFilter,
    /// Keyboard cursor across the visible vault rows.
    cursor: Option<SecurityRecord>,
    /// Focus handle for keyboard navigation on the vault lists.
    list_focus: gpui::FocusHandle,
    /// Scroll handle of the left master list in the split workbench.
    list_scroll: ScrollHandle,
    /// Scroll handle of the right detail pane in the split workbench.
    detail_scroll: ScrollHandle,
    /// Pending connection-dialog target to bind when an inline creation finishes.
    pending_bind: Option<PendingVaultBind>,
    /// The record whose detail card is drawn.
    detail: Option<SecurityRecord>,
    /// Whether the detail card shows the password. Reset by every move, so a
    /// reveal never survives into the next record or the next visit.
    revealed: bool,
    /// The one dialog the page has open.
    dialog: Option<SecurityDialog>,
    /// The new public key a finished rotation asks the user to deploy, drawn
    /// until it is dismissed.
    rotation_public_key: Option<String>,
    /// The connection book as it was when the running job started - SSH hosts
    /// and desktop connections alike - kept so the job's reference changes can
    /// be replayed onto the book as it stands when the job ends, instead of
    /// overwriting edits made meanwhile.
    job_book: Option<Vec<ConnectionRecord>>,
    /// The scroll handle of the open dialog's body.
    dialog_scroll: ScrollHandle,
    /// This window's name as a writer of vault records, drawn once per run.
    ///
    /// It is the same identity the sync state keeps (`sync-state.json`), so a
    /// record this window writes and a vault a sync merges agree on who wrote
    /// what, and two app runs never invent two identities for one machine.
    device: String,
    /// Monotonic generation bumped on every vault open or lock, preventing stale
    /// background unseals from populating `content` after the user locked.
    vault_epoch: u64,
    /// Monotonic generation bumped on every system verification or lock, preventing
    /// stale verification callbacks from mutating a newer lock state.
    verify_epoch: u64,
    /// In-place verification cancellation/failure reason per desktop session, shown
    /// directly on the session's failure card without expanding raw diagnostics.
    session_verify_failures: std::collections::HashMap<shellune_core::SessionId, String>,
    /// Whether a test holds the finish of one job, to draw the busy state the
    /// window shows while work is in flight (the same shape as the sync
    /// panel's held finish).
    #[cfg(test)]
    hold_finish: bool,
    /// The held job result, until the test releases it.
    #[cfg(test)]
    held: Option<SecurityResult>,
    /// Holds the finish of a system verification so tests can assert the waiting UI.
    #[cfg(test)]
    hold_verify: bool,
    /// The held verification answer and its epoch, until the test releases it.
    #[cfg(test)]
    held_verify: Option<(u64, Result<(), BiometricError>)>,
    /// Holds the finish of `vault_open_quietly` so tests can assert warming/lock races.
    #[cfg(test)]
    hold_open: bool,
    /// The held background vault open result and its epoch, until the test releases it.
    #[cfg(test)]
    held_open: Option<(u64, Result<Option<VaultContent>, String>)>,
    /// Remembered host passwords waiting for the next vault job (ADR-4): a form
    /// save, a migration and a deletion each add theirs, and one job writes
    /// them all, so a few hundred changes are one save of the vault.
    pending_host_secrets: BTreeMap<HostId, PendingHostSecret>,
    /// The host form whose save waits for its password to be in the vault
    /// (R1): its record is written only once the vault holds the password.
    host_form_save: Option<HostFormSave>,
    /// The host password job running now, when the running job is one.
    host_job: Option<HostSecretsJob>,
    /// Hosts whose old `host-<uuid>` entry is deleted once the vault holds
    /// their password (R3). An entry whose deletion failed stays here and is
    /// tried again after the next vault job.
    legacy_forget: BTreeSet<HostId>,
    /// The passwords the running batch read from old entries, moved into the
    /// vault as one job when the run ends (ADR-5).
    batch_migrations: Option<crate::app::batch::BatchMigrations>,
    /// A backup import is writing the vault file: no vault job starts until
    /// it ends, so neither save drops what the other wrote.
    import_writing: bool,
    /// A backup import that waits for the running vault job to end.
    import_waiting: Option<VaultWriterTask>,
    /// A sync job runs and may write the vault file: no vault job starts
    /// until it ended and its vault was re-read, so a host password save
    /// cannot drop the passwords the sync pulled (R4).
    sync_writing: bool,
}

/// Work that writes the vault outside a vault job, started once none runs.
pub(crate) type VaultWriterTask = Box<dyn FnOnce(&mut ShelluneView, &mut Context<ShelluneView>)>;

/// One remembered host password change waiting for the vault.
#[derive(Clone, Debug)]
pub(crate) enum PendingHostSecret {
    /// The vault keeps this password for the host.
    Set(SecretString),
    /// The vault deletes the host's password (a tombstone).
    Remove,
}

/// The change a host or connection form saves.
pub(crate) enum HostFormEdit {
    /// An SSH host.
    Ssh(shellune_storage::HostEdit),
    /// A connection of another protocol.
    Connection(crate::app::hosts::ConnectionEdit),
}

impl HostFormEdit {
    /// The record the form saves, as the connection book holds it.
    fn record(&self) -> ConnectionRecord {
        match self {
            Self::Ssh(edit) => ConnectionRecord::Ssh(edit.host().clone()),
            Self::Connection(edit) => edit.record().clone(),
        }
    }
}

/// A form save that waits for the vault to hold the password it remembers.
pub(crate) struct HostFormSave {
    /// What the form saves.
    pub(crate) edit: HostFormEdit,
    /// The record the form was opened on, `None` for a new one: the finish
    /// only closes the form that asked.
    pub(crate) editing: Option<HostId>,
    /// Whether the record is connected to once it is written.
    pub(crate) connect: bool,
}

/// What one host password job carries.
struct HostSecretsJob {
    /// The changes it writes.
    changes: BTreeMap<HostId, PendingHostSecret>,
    /// The form save it completes, when one asked.
    form: Option<HostFormSave>,
}

/// The lock of this one device: what it asks before the vault may be read, and
/// whether it is asking right now (R3, R4, ADR-4).
///
/// The policy and the biometric switch are device-local `security.json`
/// choices that never travel. `locked` is this window's memory of the vault:
/// locking clears the opened content, unlocking verifies once with the system
/// and opens the file again. A system that cannot verify leaves the vault
/// locked; nothing here is ever treated as a pass on silence.
pub(crate) struct VaultLock {
    /// The saved choices of this machine, as they stand on disk.
    settings: SecuritySettings,
    /// Why the saved choices could not be used, when they could not.
    problem: Option<shellune_storage::SecurityProblem>,
    /// The system's user verification. The production implementation of the
    /// platform in production; the tests hand in a `ScriptedAuthenticator` so
    /// no test ever raises a real prompt.
    authenticator: Arc<dyn BiometricAuthenticator>,
    /// Whether the vault is locked on this device. Locking cleared the opened
    /// content; a new secret read needs one verification first.
    locked: bool,
    /// A system verification is running; the answer decides what continues.
    verifying: bool,
    /// What runs when the running verification succeeds, in order: the unlock
    /// first, then the one operation that asked for it. A denial or a
    /// cancellation drops the whole queue - a cancelled prompt is never a pass.
    pending: Vec<SecurityPending>,
    /// The wall-clock moment the idle lock fires. Wall clock, not a monotonic
    /// one, so a system sleep that outlasts the idle time counts as idle the
    /// moment the machine is back.
    idle_deadline: Option<SystemTime>,
    /// Bumped by every schedule and every fire; a timer only acts on its own.
    idle_generation: u64,
}

/// One step that waits for a system verification before it runs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SecurityPending {
    /// Open the vault of this machine; what follows needs it open.
    Unlock,
    /// Show the password of one record on the keys page.
    Reveal(CredentialId),
    /// Copy the password of one record, without drawing it.
    Copy(CredentialId),
    /// Continue the connection the chain form was about to dial, bound to the
    /// originating session (once parked/registered) and target host so tab
    /// switches or opening another host never lose or cross-route the verification.
    ResumeChain {
        session: Option<shellune_core::SessionId>,
        host: HostId,
        take_panel: bool,
    },
    /// Start one desktop session whose shared password the vault holds.
    Desktop(shellune_core::SessionId),
    /// Write the private-key export the open dialog has chosen a path for.
    ExportKey,
    /// Start the backup export whose vault checkbox is on, after its fresh
    /// verification (the whole vault would otherwise leave the machine).
    BackupVaultExport,
    /// Start the backup export that seals host passwords from the vault,
    /// once the vault's records can be read (R4).
    BackupSecretsExport,
    /// Turn the biometric switch on, after one fresh verification.
    EnableBiometric,
    /// Turn the biometric switch off, after one fresh verification.
    DisableBiometric,
    /// Open a security action dialog once the vault unlocks.
    OpenAction(SecurityAction),
    /// Write the remembered host passwords waiting for the vault, the form
    /// save among them (R1).
    HostSecrets,
    /// Start the confirmed batch run whose hosts remember their passwords (R2).
    Batch,
}

impl VaultLock {
    /// The lock as a fresh window holds it: the saved choices read from disk
    /// (the defaults when there are none), the platform's verifier, and a
    /// vault still to be opened.
    fn new(root: &std::path::Path) -> Self {
        let store = SecurityStore::at(root);
        let (settings, problem) = store.load_reported();
        Self {
            settings,
            problem,
            authenticator: Arc::new(SystemAuthenticator),
            locked: false,
            verifying: false,
            pending: Vec::new(),
            idle_deadline: None,
            idle_generation: 0,
        }
    }

    /// Whether the user turned biometric unlocking on for this machine,
    /// chose a policy that re-locks, and the system still offers a
    /// verification to ask for. Everything the lock does - the start-up lock,
    /// the idle timer, the per-operation gate - hangs off this one answer.
    ///
    /// The capability is part of it on purpose (P1-2): a machine whose system
    /// has nothing this app can call - the sensor vanished, the service is
    /// gone - must never start locked behind a prompt that cannot be raised;
    /// the unlocked session is the whole protection it has (R3, R4, and
    /// `LocalOnlyFallback` in the storage layer). The query is silent, so
    /// asking it here never surprises anyone.
    fn engaged(&self) -> bool {
        self.settings.biometric_unlock == Some(true)
            && !self.settings.policy.is_disabled()
            && self.authenticator.capability().can_verify_user()
    }

    /// Whether the biometric switch is on in the shape that has no system
    /// verification behind it: enabled on the unlocked session after an
    /// explicit risk confirmation, and never auto-locked by start-up or idle
    /// (`LocalOnlyFallback::UnlockedSessionWithConfirmation`, R4).
    fn session_only(&self) -> bool {
        self.settings.biometric_unlock == Some(true)
            && !self.authenticator.capability().can_verify_user()
    }

    /// Whether every sensitive operation verifies afresh, however long the
    /// vault has been unlocked. One success is never the next operation's
    /// pass, so the gate asks the system again each time (R3).
    fn verifies_every_operation(&self) -> bool {
        self.settings.biometric_unlock == Some(true)
            && self.settings.policy == LockPolicy::EverySensitiveOperation
    }

    /// The wall-clock idle time the settings ask for.
    fn idle_lock(&self) -> Duration {
        self.settings.idle_lock()
    }
}

/// Which record of the vault the detail card is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecurityRecord {
    /// A managed key.
    Key(KeyId),
    /// A shared password.
    Credential(CredentialId),
}

/// The page's one dialog at a time. The fields are created when the dialog
/// opens, so a keystroke redraws the frame without losing the caret.
pub(crate) enum SecurityDialog {
    /// Paste an OpenSSH private key and give it a name.
    ImportKey {
        /// The pasted key text.
        pem: Entity<TextField>,
        /// The key's own passphrase, when the file was encrypted.
        passphrase: Entity<TextField>,
        /// The name the record goes by.
        name: Entity<TextField>,
        /// The last refusal, drawn above the actions.
        refusal: Option<String>,
    },
    /// Generate a key of a chosen kind.
    GenerateKey {
        /// The kind to generate.
        kind: keys_api::ManagedKeyKind,
        /// The name the record goes by.
        name: Entity<TextField>,
        /// The comment of the `authorized_keys` line, optional.
        comment: Entity<TextField>,
        /// The last refusal.
        refusal: Option<String>,
    },
    /// Edit the name and the comment of a key; neither touches the material.
    EditKey {
        /// The key being edited.
        key: KeyId,
        /// The name field.
        name: Entity<TextField>,
        /// The comment field.
        comment: Entity<TextField>,
        /// The last refusal.
        refusal: Option<String>,
    },
    /// Export one private key, encrypted with a one-time passphrase by default.
    ExportKey {
        /// The key being exported.
        key: KeyId,
        /// The export passphrase, used for this export only.
        passphrase: Entity<TextField>,
        /// Its confirmation.
        confirm: Entity<TextField>,
        /// Whether a plain, unencrypted export was chosen.
        plain: bool,
        /// The second, explicit acceptance of a plain export's risk.
        plain_confirmed: bool,
        /// Whether the system's save dialog is open.
        choosing: bool,
        /// The last refusal.
        refusal: Option<String>,
        /// Where a finished export went, until the dialog closes.
        saved_to: Option<String>,
    },
    /// Move chosen connections onto a newly generated key.
    RotateKey {
        /// The key those connections use today.
        previous: KeyId,
        /// The kind of the new key.
        kind: keys_api::ManagedKeyKind,
        /// The new key's name.
        name: Entity<TextField>,
        /// The connections that can move, and which of them are chosen.
        hosts: Vec<(HostId, bool)>,
        /// Whether the previous key is deleted once nothing uses it.
        retire: bool,
        /// The last refusal.
        refusal: Option<String>,
    },
    /// Delete a key after its affected connections were shown.
    DeleteKey {
        /// The key to delete.
        key: KeyId,
        /// The connections that sign in with it today.
        affected: Vec<HostId>,
    },
    /// Create a shared password, typed or generated.
    NewCredential {
        /// The name the record goes by.
        name: Entity<TextField>,
        /// The optional login user the credential stands for.
        user: Entity<TextField>,
        /// The password itself, masked.
        secret: Entity<TextField>,
        /// The chosen length of a generated password.
        length: Entity<TextField>,
        /// Whether letters are part of a generated password.
        letters: bool,
        /// Whether digits are part of one.
        digits: bool,
        /// Whether symbols are part of one.
        symbols: bool,
        /// The last refusal.
        refusal: Option<String>,
    },
    /// Edit a shared password; an empty password field keeps the current one.
    EditCredential {
        /// The credential being edited.
        credential: CredentialId,
        /// The name field.
        name: Entity<TextField>,
        /// The user field.
        user: Entity<TextField>,
        /// The new password, empty to keep the current one.
        secret: Entity<TextField>,
        /// The chosen length of a generated password.
        length: Entity<TextField>,
        /// Whether letters are part of a generated password.
        letters: bool,
        /// Whether digits are part of one.
        digits: bool,
        /// Whether symbols are part of one.
        symbols: bool,
        /// The last refusal.
        refusal: Option<String>,
    },
    /// Delete a shared password after its affected connections were shown.
    DeleteCredential {
        /// The credential to delete.
        credential: CredentialId,
        /// The connections that sign in with it today.
        affected: Vec<HostId>,
    },
    /// Remove one accepted server identity (R7). No password is asked for: the
    /// record is public fact, and the vault this device can already unseal is
    /// the only thing the change touches.
    DeleteTrust {
        /// The server name or address.
        host: String,
        /// The server port.
        port: u16,
    },
    /// The explicit risk confirmation behind enabling the biometric switch on
    /// a machine whose system offers no verification this app can call: the
    /// unlocked session is then the whole protection, and the page says so in
    /// exactly those words (R4, `LocalOnlyFallback`).
    BiometricRisk,
}

impl SecurityDialog {
    /// Whether `handle` belongs to one of the text inputs owned by this open dialog.
    pub(crate) fn owns_focus(&self, handle: &gpui::FocusHandle, cx: &App) -> bool {
        match self {
            Self::ImportKey {
                pem,
                passphrase,
                name,
                ..
            } => [pem, passphrase, name]
                .into_iter()
                .any(|field| field.read(cx).focus_handle() == handle),
            Self::GenerateKey { name, comment, .. } | Self::EditKey { name, comment, .. } => {
                [name, comment]
                    .into_iter()
                    .any(|field| field.read(cx).focus_handle() == handle)
            }
            Self::ExportKey {
                passphrase,
                confirm,
                ..
            } => [passphrase, confirm]
                .into_iter()
                .any(|field| field.read(cx).focus_handle() == handle),
            Self::RotateKey { name, .. } => name.read(cx).focus_handle() == handle,
            Self::NewCredential {
                name,
                user,
                secret,
                length,
                ..
            }
            | Self::EditCredential {
                name,
                user,
                secret,
                length,
                ..
            } => [name, user, secret, length]
                .into_iter()
                .any(|field| field.read(cx).focus_handle() == handle),
            Self::DeleteKey { .. }
            | Self::DeleteCredential { .. }
            | Self::DeleteTrust { .. }
            | Self::BiometricRisk => false,
        }
    }
}

impl SecurityPage {
    /// The page's fields, with its searches created and nothing read yet.
    ///
    /// The lock choices come from `security.json` beside the data, and the
    /// device name is the one the sync state keeps: the two are created once
    /// here so the whole run answers with one identity (ADR-5).
    pub(crate) fn new(root: &std::path::Path, cx: &mut Context<ShelluneView>) -> Self {
        let strings = copy::security_for(Locale::current());
        let placeholder = strings.search_placeholder;
        let search = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_leading_icon(Icon::Search, cx);
            field.set_clearable(true, cx);
            field
        });
        let list_focus = cx.focus_handle().tab_stop(true);
        let search_tab_view = cx.entity().downgrade();
        search.update(cx, |field, _cx| {
            field.on_tab(move |shift, window, app| {
                if shift {
                    return false;
                }
                let can_focus = search_tab_view
                    .read_with(app, |view, _app| {
                        !view.vault_locked() && view.security_record_count() > 0
                    })
                    .unwrap_or(false);
                if !can_focus {
                    return false;
                }
                let target_view = search_tab_view.clone();
                window.defer(app, move |window, app| {
                    let _ = target_view.update(app, |view, cx| {
                        view.security_focus_list(window, cx);
                    });
                });
                true
            });
        });
        cx.observe(&search, |this, _, cx| {
            this.security.revealed = false;
            this.security.cursor = None;
            cx.notify();
        })
        .detach();
        let trust_search = cx.new(|cx| {
            let mut field = TextField::new(strings.trust_search_placeholder, cx);
            field.set_leading_icon(Icon::Search, cx);
            field.set_clearable(true, cx);
            field
        });
        cx.observe(&trust_search, |_, _, cx| cx.notify()).detach();
        let lock = VaultLock::new(root);
        let idle = lock
            .settings
            .idle_lock_minutes
            .unwrap_or(shellune_storage::DEFAULT_IDLE_LOCK_MINUTES)
            .to_string();
        let idle_field = cx.new(|cx| TextField::with_text("", idle, cx));
        // The idle minutes save on submit; a keystroke alone changes nothing,
        // the way the sync form works.
        let view = cx.entity().downgrade();
        idle_field.update(cx, |field, _cx| {
            field.on_submit(move |text, _window, app| {
                view.update(app, |view, cx| view.security_set_idle_minutes(&text, cx))
                    .unwrap_or(false)
            });
        });
        Self {
            search,
            trust_search,
            idle_field,
            content: None,
            lock,
            operation_granted: false,
            busy: false,
            warming: false,
            open_problem: None,
            notice: None,
            filter: SecurityFilter::All,
            cursor: None,
            list_focus,
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            pending_bind: None,
            detail: None,
            revealed: false,
            dialog: None,
            rotation_public_key: None,
            job_book: None,
            dialog_scroll: ScrollHandle::new(),
            device: device_name(root),
            vault_epoch: 0,
            verify_epoch: 0,
            session_verify_failures: std::collections::HashMap::new(),
            #[cfg(test)]
            hold_finish: false,
            #[cfg(test)]
            held: None,
            #[cfg(test)]
            hold_verify: false,
            #[cfg(test)]
            held_verify: None,
            #[cfg(test)]
            hold_open: false,
            #[cfg(test)]
            held_open: None,
            pending_host_secrets: BTreeMap::new(),
            host_form_save: None,
            host_job: None,
            legacy_forget: BTreeSet::new(),
            batch_migrations: None,
            import_writing: false,
            import_waiting: None,
            sync_writing: false,
        }
    }

    /// The vault as this window last read or wrote it, when it is unlocked.
    pub(crate) fn content(&self) -> Option<&VaultContent> {
        self.content.as_ref()
    }

    /// The name this window writes vault records under.
    pub(crate) fn device(&self) -> &str {
        &self.device
    }

    /// Whether a vault job, the start-up open, or a backup import or a sync
    /// writing the vault is in flight.
    pub(crate) fn busy(&self) -> bool {
        self.busy || self.warming || self.import_writing || self.sync_writing
    }

    /// The remembered password of a host as the window sees it: a change not
    /// written yet, else the vault's record.
    #[cfg(test)]
    pub(crate) fn host_secret_for_test(&self, id: HostId) -> Option<SecretString> {
        let waiting = self
            .pending_host_secrets
            .get(&id)
            .or_else(|| self.host_job.as_ref().and_then(|job| job.changes.get(&id)));
        match waiting {
            Some(PendingHostSecret::Set(secret)) => Some(secret.clone()),
            Some(PendingHostSecret::Remove) => None,
            None => self
                .content
                .as_ref()
                .and_then(|content| content.host_secret(id))
                .cloned(),
        }
    }
}

/// The device name this window writes vault records under: the one
/// `sync-state.json` keeps, created and persisted on the first run that finds
/// none, so the window and the sync engine name one writer (ADR-5).
fn device_name(root: &std::path::Path) -> String {
    let store = shellune_sync::SyncStateStore::at(root);
    let mut state = store.load();
    if let Some(device) = state.device_id {
        return device;
    }
    // The same shape the sync engine gives its own fresh identities, drawn
    // from the same random identifier source the core types use, so the two
    // spellings of one device cannot be told apart.
    let device = shellune_core::TransferId::new()
        .as_uuid()
        .simple()
        .to_string();
    state.device_id = Some(device.clone());
    // A state file that cannot be written still leaves this run one identity;
    // the next run makes its own, and the versions only ever needed to tell
    // writers apart.
    let _ = store.save(&state);
    device
}

// ---------------------------------------------------------------------------
// What one background job did
// ---------------------------------------------------------------------------

/// What one vault job changed, in words the page can show without a secret.
pub(crate) enum SecurityReport {
    /// A key record was created (import or generation) or its metadata edited.
    KeySaved(KeyId),
    /// A shared password record was created.
    CredentialSaved(CredentialId),
    /// A shared password was edited; every reference now uses the new value.
    CredentialEdited,
    /// An independent copy of a shared password was created.
    CredentialDuplicated,
    /// A key record was deleted.
    KeyDeleted,
    /// A password record was deleted.
    CredentialDeleted,
    /// One host stopped referencing a record.
    Unbound {
        /// The host that was unbound, by name.
        host: String,
    },
    /// Connections moved onto a newly generated key.
    Rotated {
        /// How many connections moved.
        count: usize,
        /// The new public key, for the user to deploy.
        public_key: String,
    },
    /// A private key was exported to a file.
    Exported {
        /// Where the export went.
        path: PathBuf,
    },
    /// An import was accepted with a warning worth reading.
    ImportedWithWarning {
        /// The imported key's identity.
        id: KeyId,
        /// The warning shown after import.
        warning: String,
    },
    /// An accepted server identity was removed (R7).
    TrustForgotten {
        /// The server it was for, as the page names it.
        server: String,
    },
    /// An identity accepted at a connection prompt was mirrored into the
    /// vault, so it travels with it (ADR-6). Quiet by design: the connection
    /// already said what it did.
    TrustMirrored,
    /// Remembered host passwords were written (ADR-4). Quiet: the form or
    /// the connection that asked says what happened.
    HostSecretsSaved,
}

/// How one edit wants the vault written.
enum SecurityChange {
    /// Save the vault. `records` holds only the records the edit itself
    /// changed, in their after state - an SSH host wrapped as its connection
    /// record, a desktop record as itself: they are merged onto the connection
    /// book the job saw for the save, and replayed onto the book as it stands
    /// when the job ends, so an edit made meanwhile is kept rather than
    /// quietly reverted.
    Save {
        /// What the job did.
        report: SecurityReport,
        /// The changed records, after the edit.
        records: Vec<ConnectionRecord>,
    },
    /// Nothing is written; the report is all the page needs.
    ReadOnly(SecurityReport),
}

/// One edit to apply to the opened vault, run off the UI thread.
///
/// The host book and the connections of the other protocols arrive as the two
/// lists the book keeps them in: an edit that unbinds or deletes has to see
/// every record that can name a vault record, whatever its protocol.
type SecurityEdit = Box<
    dyn FnOnce(
            &mut VaultContent,
            &[HostConfig],
            &[ConnectionRecord],
        ) -> Result<SecurityChange, String>
        + Send,
>;

/// The whole outcome of one background vault job.
enum SecurityResult {
    /// The vault was saved; the records are the book the job saved against.
    Applied {
        /// The content that was saved, as the new in-memory mirror.
        content: VaultContent,
        /// The content as it was before, for the rollback when the host file
        /// cannot be written.
        previous: Box<VaultContent>,
        /// The whole connection book as the job saved it against.
        records: Vec<ConnectionRecord>,
        /// Which records the edit changed, by identity.
        changed: Vec<HostId>,
        /// What the job did.
        report: SecurityReport,
    },
    /// The vault refused the change; nothing was written.
    Refused(String),
    /// The vault could not be opened or saved; nothing was written.
    Failed(String),
}

/// One row of a list: everything the list shows, and no secret.
struct SecurityRow {
    record: SecurityRecord,
    name: String,
    caption: String,
    chip: String,
    hosts: usize,
    host_names: Vec<String>,
    public_key: Option<String>,
    user: Option<String>,
    created: SystemTime,
}

/// One row of the host-trust list: the public facts of one accepted identity
/// (R7). Everything here may be drawn, copied and logged - none of it secret.
struct TrustRow {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
    accepted: u64,
    /// Whether the vault holds the record (the authority a sync merges) or
    /// only this device's legacy trust file does.
    from_vault: bool,
}

/// The immediate actions of the page and its detail cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecurityAction {
    /// Open the import dialog.
    Import,
    /// Open the generate dialog.
    Generate,
    /// Open the new-password dialog.
    NewCredential,
    /// Open the edit dialog of one key.
    EditKey(KeyId),
    /// Open the export dialog of one key.
    ExportKey(KeyId),
    /// Open the rotate dialog of one key.
    RotateKey(KeyId),
    /// Open the delete question of one key.
    DeleteKey(KeyId),
    /// Open the edit dialog of one credential.
    EditCredential(CredentialId),
    /// Copy one credential into an independent record at once.
    DuplicateCredential(CredentialId),
    /// Open the delete question of one credential.
    DeleteCredential(CredentialId),
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

impl ShelluneView {
    fn security_strings(&self) -> &'static SecurityStrings {
        copy::security_for(self.workspace.locale())
    }

    /// The whole page in Settings: single-column with a collapsible detail card.
    pub(super) fn security_page(&self, cx: &mut Context<Self>) -> Div {
        self.security_workbench(false, cx)
    }

    /// The first-class vault workbench mounted inside the home page's main card.
    pub(super) fn home_vault_main(
        &self,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let ux = copy::ux();
        let folded = self.home_sidebar_folded();
        let toggle = components::icon_only_button(Icon::Sidebar, metrics)
            .id("home.sidebar.toggle")
            .debug_selector(|| "home.sidebar.toggle".to_owned())
            .tooltip(components::tooltip(if folded {
                ux.home_sidebar_expand
            } else {
                ux.home_sidebar_collapse
            }))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_home_sidebar(cx);
            }));

        let count_or_state = if self.security.lock.locked {
            components::chip(strings.vault_locked_badge, Tone::Warning, metrics)
                .debug_selector(|| "header.count".to_owned())
                .into_any_element()
        } else {
            let total = self.security_record_count();
            div()
                .flex_none()
                .px(metrics.scale(spacing::TIGHT))
                .py(metrics.scale(spacing::HALF))
                .rounded_full()
                .bg(theme::swatch(palette::SURFACE_SELECTED))
                .text_size(metrics.caption_size())
                .font_weight(metrics.medium_weight())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "header.count".to_owned())
                .child(components::line_text(total.to_string()))
                .into_any_element()
        };

        let mut toolbar_right = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::HALF));
        if self.security_topbar_lock_shown() {
            let locked = self.security.lock.locked;
            let verifying = self.security.lock.verifying;
            let label = if verifying {
                strings.lock_state_verifying
            } else if locked {
                strings.lock_unlock
            } else {
                strings.lock_now
            };
            toolbar_right = toolbar_right.child(
                components::icon_label_button(
                    if locked { Icon::Lock } else { Icon::Unlock },
                    label,
                    if locked {
                        Variant::Primary
                    } else {
                        Variant::Ghost
                    },
                    metrics,
                )
                .h(metrics.compact_control_height())
                .id("security.toolbar.lock")
                .debug_selector(|| "security.toolbar.lock".to_owned())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if locked {
                        this.security_begin(Vec::new(), cx);
                    } else {
                        this.security_lock_now(cx);
                    }
                })),
            );
        }

        let toolbar = components::toolbar_row(metrics)
            .debug_selector(|| "security.toolbar".to_owned())
            .flex_wrap()
            .child(toggle)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics.heading_size())
                            .font_weight(metrics.title_weight())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "header.title".to_owned())
                            .child(components::line_text(strings.vault_nav_title)),
                    )
                    .child(count_or_state),
            )
            .child(toolbar_right);

        let independent_split = self.home_vault_independent_split();

        div()
            .id("home.vault")
            .debug_selector(|| "home.vault".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .child(toolbar),
            )
            .child(
                div()
                    .id("home.vault.scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(independent_split, |col| col.overflow_hidden())
                    .when(!independent_split, |col| {
                        col.overflow_y_scroll().overflow_x_hidden()
                    })
                    .px(metrics.scale(spacing::WIDE))
                    .py(metrics.scale(spacing::BASE))
                    .child(self.security_workbench(true, cx)),
            )
    }

    /// Whether the Home vault area is wide enough for the two-column master-detail split.
    fn home_vault_wide_split(&self) -> bool {
        let metrics = self.metrics();
        let sidebar_room = if self.home_sidebar_folded() {
            super::home::SIDEBAR_FOLDED_WIDTH
        } else {
            super::home::SIDEBAR_WIDTH
        };
        (self.viewport_width - metrics.scale(sidebar_room + spacing::TIGHT))
            >= metrics.scale(VAULT_SPLIT_MIN_WIDTH)
    }

    /// Whether the Home vault area should lock outer vertical scrolling so the left list
    /// and right detail pane scroll independently within the viewport.
    fn home_vault_independent_split(&self) -> bool {
        !self.security.lock.locked
            && self.viewport_height >= self.metrics().scale(super::home::SHORT_HEIGHT)
            && self.home_vault_wide_split()
    }

    /// Shared vault workbench body mounted in both Home (`in_home == true`) and Settings.
    fn security_workbench(&self, in_home: bool, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let independent_split = in_home && self.home_vault_independent_split();
        let mut page = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .when(in_home, |el| el.flex_1().min_h_0())
            .when(independent_split, |el| el.overflow_hidden())
            .gap(metrics.scale(spacing::BASE))
            .debug_selector(|| "security.page".to_owned())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.security_workbench_capture_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape"
                    && event.keystroke.modifiers.number_of_modifiers() == 0
                    && this.security_close_detail(cx)
                {
                    cx.stop_propagation();
                }
            }));

        if let Some(reason) = &self.security.open_problem {
            page = page.child(
                components::banner(Tone::Danger, metrics)
                    .debug_selector(|| "security.open.problem".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(format!(
                        "{}: {reason}",
                        strings.vault_failed
                    ))),
            );
        }
        if let Some((tone, text)) = &self.security.notice {
            page = page.child(
                components::banner(*tone, metrics)
                    .debug_selector(|| "security.notice".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(text.clone())),
            );
        }
        if self.security.busy() {
            page = page.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "security.busy".to_owned())
                    .child(strings.busy),
            );
        }
        if self.security.lock.locked {
            // A locked vault shows only banners and the unlock card — never active
            // creation controls or misleading "no keys / no passwords" empty lists (R2).
            let locked = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .gap(metrics.scale(spacing::TIGHT))
                .p(metrics.scale(spacing::BASE))
                .rounded(px(radius::MEDIUM))
                .border_1()
                .border_color(theme::swatch(palette::BORDER))
                .debug_selector(|| "security.locked".to_owned())
                .child(
                    div()
                        .text_size(metrics.body_size())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(strings.lock_state_locked),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .whitespace_normal()
                        .child(strings.lock_blocked),
                )
                .child(
                    components::compact_button(
                        if self.security.lock.verifying {
                            strings.lock_state_verifying
                        } else {
                            strings.lock_unlock
                        },
                        Variant::Primary,
                        metrics,
                    )
                    .id("security.locked.unlock")
                    .debug_selector(|| "security.locked.unlock".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.security_begin(Vec::new(), cx);
                    })),
                );
            return page.child(locked);
        }
        if let Some(public_key) = &self.security.rotation_public_key {
            page = page.child(self.security_rotation_result(public_key, cx));
        }

        let disabled = self.security.busy() || self.security.dialog.is_some();
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "security.actions".to_owned());
        for (selector, label, icon, action) in [
            (
                "security.action.import",
                strings.import_key,
                Icon::Import,
                SecurityAction::Import,
            ),
            (
                "security.action.generate",
                strings.generate_key,
                Icon::Plus,
                SecurityAction::Generate,
            ),
            (
                "security.action.new-password",
                strings.new_password,
                Icon::Lock,
                SecurityAction::NewCredential,
            ),
        ] {
            let button = components::icon_label_button(icon, label, Variant::Secondary, metrics)
                .id(SharedString::from(selector))
                .debug_selector(move || selector.to_owned());
            actions = actions.child(if disabled {
                button
            } else {
                button.on_click(cx.listener(move |this, _event, _window, cx| {
                    this.security_start(action, cx);
                }))
            });
        }

        let mut filters = components::segmented(metrics)
            .flex_wrap()
            .max_w_full()
            .debug_selector(|| "security.filter".to_owned());
        for filter in SecurityFilter::ALL {
            let chosen = self.security.filter == filter;
            let selector = filter.selector();
            filters = filters.child(
                components::segment(filter.label(strings), chosen, metrics)
                    .id(SharedString::from(selector))
                    .debug_selector(move || selector.to_owned())
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.security_set_filter(filter, cx);
                    })),
            );
        }

        page = page
            .child(actions)
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(|| "security.search".to_owned())
                    .child(self.security.search.clone()),
            )
            .child(div().flex().min_w_0().max_w_full().child(filters));

        let wide_split = in_home && self.home_vault_wide_split();

        let lists_column = self
            .security_filtered_lists(cx)
            .track_focus(&self.security.list_focus)
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "security.secrets.hidden".to_owned())
                    .child(strings.secrets_hidden),
            );

        if wide_split {
            let detail_pane = match self
                .security
                .detail
                .and_then(|record| self.security_detail(record, cx))
            {
                Some(card) => card,
                None => components::card(metrics)
                    .w_full()
                    .p(metrics.scale(spacing::BASE))
                    .debug_selector(|| "security.detail.placeholder".to_owned())
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.detail_select_hint),
                    ),
            };
            page.child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(independent_split, |split| split.overflow_hidden())
                    .when(!independent_split, |split| split.items_start())
                    .gap(metrics.scale(spacing::BASE))
                    .debug_selector(|| "security.split".to_owned())
                    .child(
                        div()
                            .id("security.list.scroll")
                            .track_scroll(&self.security.list_scroll)
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .when(independent_split, |col| {
                                col.min_h_0()
                                    .h_full()
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                            })
                            .when(!independent_split, |col| col.max_h_full())
                            .child(lists_column),
                    )
                    .child(
                        div()
                            .id("security.detail.scroll")
                            .track_scroll(&self.security.detail_scroll)
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(metrics.scale(VAULT_DETAIL_WIDTH))
                            .max_w(gpui::relative(0.46))
                            .min_w_0()
                            .when(independent_split, |col| {
                                col.min_h_0()
                                    .h_full()
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                            })
                            .when(!independent_split, |col| col.max_h_full())
                            .child(detail_pane),
                    ),
            )
        } else {
            if let Some(record) = self.security.detail
                && let Some(card) = self.security_detail(record, cx)
            {
                page = page.child(card);
            }
            page.child(lists_column)
        }
    }

    /// Renders the key and/or shared password lists matching the active [`SecurityFilter`].
    fn security_filtered_lists(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let mut col = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::BASE));
        match self.security.filter {
            SecurityFilter::All => {
                col = col
                    .child(self.security_list(true, cx))
                    .child(self.security_list(false, cx));
            }
            SecurityFilter::Keys => {
                col = col.child(self.security_list(true, cx));
            }
            SecurityFilter::Credentials => {
                col = col.child(self.security_list(false, cx));
            }
            SecurityFilter::Unused => {
                let key_rows = self.security_rows(true, cx);
                let cred_rows = self.security_rows(false, cx);
                if key_rows.is_empty() && cred_rows.is_empty() {
                    let narrowed = !self.security.search.read(cx).text().trim().is_empty();
                    col = col.child(
                        div()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "security.unused.empty".to_owned())
                            .child(if narrowed {
                                strings.no_match
                            } else {
                                strings.empty_unused
                            }),
                    );
                } else {
                    if !key_rows.is_empty() {
                        col = col.child(self.security_list(true, cx));
                    }
                    if !cred_rows.is_empty() {
                        col = col.child(self.security_list(false, cx));
                    }
                }
            }
        }
        col
    }

    /// The public key a finished rotation asks the user to deploy, with the
    /// one way to take it away.
    fn security_rotation_result(&self, public_key: &str, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .p(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::MEDIUM))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "security.rotation.result".to_owned())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .flex_wrap()
                    .child(
                        div()
                            .text_size(metrics.label_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.rotate_public),
                    )
                    .child(
                        components::compact_button(strings.close, Variant::Ghost, metrics)
                            .id("security.rotation.dismiss")
                            .debug_selector(|| "security.rotation.dismiss".to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.security.rotation_public_key = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                components::copy_on_click(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .font_family(crate::app::terminal::monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .debug_selector(|| "security.rotation.public".to_owned()),
                    public_key.to_owned(),
                )
                .child(SharedString::from(public_key.to_owned())),
            )
    }

    /// The security category's host-trust page: the accepted server identities
    /// of the vault and of this device's legacy file, searchable, each with its
    /// full fingerprint to copy and its removal one confirmation away (R7).
    pub(super) fn trust_page(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let mut page = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::BASE))
            .debug_selector(|| "security.trust.page".to_owned())
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(|| "security.trust.search".to_owned())
                    .child(self.security.trust_search.clone()),
            );
        if let Some((tone, text)) = &self.security.notice {
            page = page.child(
                components::banner(*tone, metrics)
                    .debug_selector(|| "security.notice".to_owned())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(text.clone())),
            );
        }
        if self.security.busy() {
            page = page.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "security.busy".to_owned())
                    .child(strings.busy),
            );
        }
        if self.security.lock.locked {
            page = page.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::TIGHT))
                    .w_full()
                    .min_w_0()
                    .p(metrics.scale(spacing::BASE))
                    .rounded(px(radius::MEDIUM))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .bg(Tone::Warning.chip_background())
                    .debug_selector(|| "security.trust.locked".to_owned())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(strings.trust_locked_hint),
                    )
                    .child(
                        components::compact_button(
                            if self.security.lock.verifying {
                                strings.lock_state_verifying
                            } else {
                                strings.lock_unlock
                            },
                            Variant::Primary,
                            metrics,
                        )
                        .id("security.trust.unlock")
                        .debug_selector(|| "security.trust.unlock".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_begin(Vec::new(), cx);
                            },
                        )),
                    ),
            );
        }
        page.child(self.trust_list(cx))
    }

    /// The trust list: one row per accepted identity, narrowed by the search.
    fn trust_list(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let locale = self.workspace.locale();
        let column = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "security.trust.list".to_owned())
            .child(
                div()
                    .text_size(metrics.body_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "security.trust.title".to_owned())
                    .child(strings.trust_title),
            );
        let rows = self.trust_rows(cx);
        if rows.is_empty() {
            let narrowed = !self.security.trust_search.read(cx).text().trim().is_empty();
            return column.child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "security.trust.empty".to_owned())
                    .child(if narrowed {
                        strings.trust_no_match
                    } else {
                        strings.trust_empty
                    }),
            );
        }
        let busy = self.security.busy() || self.security.dialog.is_some();
        let count = rows.len();
        let mut card = components::card(metrics).w_full().p(px(0.0)).gap(px(0.0));
        for (index, row) in rows.into_iter().enumerate() {
            let TrustRow {
                host,
                port,
                algorithm,
                fingerprint,
                accepted,
                from_vault,
            } = row;
            let server = format!("{host}:{port}");
            let accepted_line = copy::fill(
                strings.trust_accepted,
                &[("date", &copy::relative_time(accepted, remote_now(), locale))],
            );
            let source_line = if from_vault {
                strings.trust_from_vault
            } else {
                strings.trust_from_this_mac
            };
            let row_host = host.clone();
            let row_port = port;
            let delete = components::compact_button(
                strings.trust_delete_action,
                Variant::Secondary,
                metrics,
            )
            .id(SharedString::from(format!("security.trust.delete.{index}")))
            .debug_selector(move || format!("security.trust.delete.{index}"))
            .when(!busy, |button| {
                button.on_click(cx.listener(move |this, _event, _window, cx| {
                    if this.security.busy() || this.security.dialog.is_some() {
                        return;
                    }
                    this.security.dialog = Some(SecurityDialog::DeleteTrust {
                        host: row_host.clone(),
                        port: row_port,
                    });
                    cx.notify();
                }))
            });
            card = card.child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .when(index + 1 < count, |row| {
                        row.border_b_1()
                            .border_color(theme::swatch(palette::BORDER))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(metrics.scale(spacing::TIGHT))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(metrics.body_size())
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                            .debug_selector(move || {
                                                format!("security.trust.host.{index}")
                                            })
                                            .child(SharedString::from(server)),
                                    )
                                    .child(components::chip_within(
                                        SharedString::from(source_line),
                                        if from_vault {
                                            Tone::Accent
                                        } else {
                                            Tone::Neutral
                                        },
                                        metrics,
                                    )),
                            )
                            .child(
                                components::copy_on_click(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(crate::app::terminal::monospace_family())
                                        .text_size(metrics.caption_size())
                                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                        .debug_selector(move || {
                                            format!("security.trust.fingerprint.{index}")
                                        }),
                                    fingerprint,
                                )
                                .child(SharedString::from(
                                    format!("{algorithm} \u{b7} {accepted_line}"),
                                )),
                            ),
                    )
                    .child(delete),
            );
        }
        column.child(card)
    }

    /// The rows of the trust list: the vault's identities first - it is the
    /// authority - then this device's legacy entries no vault holds, narrowed
    /// by the search over host, port, algorithm and the full fingerprint.
    fn trust_rows(&self, cx: &App) -> Vec<TrustRow> {
        let vault = self.security.content.as_ref();
        let query = self
            .security
            .trust_search
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        let needles: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
        let narrowed = !needles.is_empty();
        let mut rows = Vec::new();
        let mut push = |entry: &TrustEntry, from_vault: bool| {
            let haystacks = [
                entry.host().to_lowercase(),
                entry.port().to_string(),
                entry.algorithm().to_lowercase(),
                entry.fingerprint().to_lowercase(),
            ];
            if narrowed
                && !needles
                    .iter()
                    .all(|needle| haystacks.iter().any(|text| text.contains(needle)))
            {
                return;
            }
            rows.push(TrustRow {
                host: entry.host().to_owned(),
                port: entry.port(),
                algorithm: entry.algorithm().to_owned(),
                fingerprint: entry.fingerprint().to_owned(),
                accepted: entry.accepted_unix(),
                from_vault,
            });
        };
        if let Some(vault) = vault {
            for entry in vault.trust().entries() {
                push(entry, true);
            }
        }
        let in_vault = |entry: &TrustEntry| {
            vault.is_some_and(|vault| vault.trust().entry(entry.host(), entry.port()).is_some())
        };
        for entry in self.trust_book.entries() {
            if !in_vault(entry) {
                push(entry, false);
            }
        }
        // Where a reader looks: by host, then the older acceptance first, so
        // the list a reader sees does not reshuffle between runs.
        rows.sort_by(|a, b| {
            a.host
                .cmp(&b.host)
                .then_with(|| a.port.cmp(&b.port))
                .then_with(|| a.accepted.cmp(&b.accepted))
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        });
        rows
    }

    /// The security category's lock page: the state of this device's vault
    /// lock, the biometric switch, the two protection modes and the idle lock
    /// (R3, R4, ADR-4). Everything it shows is a choice about this machine.
    pub(super) fn biometrics_page(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let lock = &self.security.lock;
        let biometric = lock.settings.biometric_unlock == Some(true);
        let status = if lock.verifying {
            strings.lock_state_verifying
        } else if lock.locked {
            strings.lock_state_locked
        } else {
            strings.lock_state_open
        };
        let status_row = self.settings_row(
            "lock-status",
            status,
            "",
            div().debug_selector(|| "security.lock.status".to_owned()),
        );
        // "Lock now" is only offered where something can unlock it again: a
        // machine whose system has no verification to ask would turn the
        // button into a one-way door (R4, P1-2).
        let action = if lock.locked && !lock.verifying {
            Some((
                "security.lock.unlock",
                strings.lock_unlock,
                Variant::Primary,
            ))
        } else if lock.verifying || !lock.engaged() {
            None
        } else {
            Some(("security.lock.now", strings.lock_now, Variant::Secondary))
        };
        let action_row = action.map(|(selector, label, variant)| {
            self.settings_row(
                "lock-action",
                "",
                "",
                div()
                    .debug_selector(move || format!("{selector}.row"))
                    .child(
                        components::compact_button(label, variant, metrics)
                            .id(SharedString::from(selector))
                            .debug_selector(move || selector.to_owned())
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if this.security.lock.locked {
                                    this.security_begin(Vec::new(), cx);
                                } else {
                                    this.security_lock_now(cx);
                                }
                            })),
                    ),
            )
        });
        // The capability is asked silently as the page draws: it decides what
        // the page may promise, it never raises a prompt, and it is not cached
        // anywhere - a system whose answer is busy this moment is not folded
        // into a permanent "unavailable" (R4, T4's handoff).
        let capability = self.security.lock.authenticator.capability();
        let capability_line = match capability {
            BiometricCapability::Biometrics => strings.lock_capability_biometrics,
            BiometricCapability::SystemUserVerification => strings.lock_capability_system,
            BiometricCapability::Unavailable => strings.lock_capability_none,
        };
        let switch = |view: &Self,
                      id: &'static str,
                      on: bool,
                      cx: &mut Context<Self>,
                      pick: fn(&mut Self, bool, &mut Context<Self>)| {
            view.switch_control(id, on, cx, pick)
        };
        let mut rows = vec![
            status_row,
            self.settings_row(
                "lock-biometric",
                strings.lock_biometric,
                capability_line,
                switch(
                    self,
                    "security.lock.biometric",
                    biometric,
                    cx,
                    Self::security_toggle_biometric,
                ),
            ),
        ];
        rows.extend(action_row);
        if biometric {
            let every = lock.settings.policy == LockPolicy::EverySensitiveOperation;
            rows.push(self.settings_row(
                "lock-policy",
                strings.lock_policy_label,
                if every {
                    strings.lock_policy_every_hint
                } else {
                    strings.lock_policy_unlock_hint
                },
                self.segments(
                    "security.lock.policy",
                    vec![
                        (strings.lock_policy_unlock.to_owned(), !every),
                        (strings.lock_policy_every.to_owned(), every),
                    ],
                    cx,
                    |view, index, cx| view.security_set_lock_policy(index == 1, cx),
                ),
            ));
            rows.push(
                self.settings_row(
                    "lock-idle",
                    strings.lock_idle_label,
                    strings.lock_idle_hint,
                    div()
                        .w(metrics.scale(IDLE_FIELD_WIDTH))
                        .max_w(components::settings_row_content_cap(metrics))
                        .debug_selector(|| "security.lock.idle".to_owned())
                        .child(self.security.idle_field.clone()),
                ),
            );
        }
        if let Some(_problem) = &lock.problem {
            rows.push(self.settings_row(
                "lock-problem",
                strings.lock_settings_problem,
                "",
                div().debug_selector(|| "security.lock.problem".to_owned()),
            ));
        }
        self.settings_card("lock", rows)
    }

    /// One of the two lists: the keys, or the shared passwords.
    fn security_list(&self, keys: bool, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let (title, selector) = if keys {
            (strings.keys_title, "security.keys")
        } else {
            (strings.credentials_title, "security.credentials")
        };
        let locale = self.workspace.locale();
        let query = self.security.search.read(cx).text().trim().to_owned();
        let busy = self.security.busy() || self.security.dialog.is_some();
        let column = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(move || selector.to_owned())
            .child(
                div()
                    .text_size(metrics.body_size())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(move || format!("{selector}.title"))
                    .child(title),
            );
        let rows = self.security_rows(keys, cx);
        if rows.is_empty() {
            let empty = if keys {
                strings.empty_keys
            } else {
                strings.empty_credentials
            };
            let narrowed = !query.is_empty();
            return column.child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(move || format!("{selector}.empty"))
                    .child(if narrowed { strings.no_match } else { empty }),
            );
        }
        let selected = self.security.detail;
        let cursor = self.security.cursor;
        let count = rows.len();
        let mut card = components::card(metrics).w_full().p(px(0.0)).gap(px(0.0));
        for (index, row) in rows.into_iter().enumerate() {
            let SecurityRow {
                record,
                name,
                caption,
                chip,
                hosts,
                host_names,
                public_key,
                user,
                created,
            } = row;
            let chosen = selected == Some(record);
            let focused = cursor == Some(record);
            let row_selector = if keys {
                format!("security.key.{index}")
            } else {
                format!("security.credential.{index}")
            };
            let hosts_line = if hosts == 0 {
                strings.not_referenced.to_owned()
            } else {
                let count_text = copy::fill(strings.hosts_count, &[("count", &hosts.to_string())]);
                let preview: Vec<&str> = host_names.iter().take(2).map(String::as_str).collect();
                if host_names.len() > 2 {
                    format!(
                        "{count_text} ({}, +{})",
                        preview.join(", "),
                        host_names.len() - 2
                    )
                } else if !preview.is_empty() {
                    format!("{count_text} ({})", preview.join(", "))
                } else {
                    count_text
                }
            };
            let created_line = copy::fill(
                strings.created,
                &[(
                    "date",
                    &copy::relative_time(unix_of(created), remote_now(), locale),
                )],
            );
            let meta_line = format!("{hosts_line} · {created_line}");

            let mut row_actions = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(metrics.scale(spacing::HAIRLINE));
            match record {
                SecurityRecord::Key(key_id) => {
                    if let Some(pub_key) = public_key {
                        let copy_sel = format!("security.key.copy.{index}");
                        row_actions = row_actions.child(
                            components::icon_only_button(Icon::Copy, metrics)
                                .id(SharedString::from(copy_sel.clone()))
                                .debug_selector({
                                    let selector = copy_sel.clone();
                                    move || selector.clone()
                                })
                                .tooltip(components::tooltip(strings.copy_public_key))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    cx.stop_propagation();
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        pub_key.clone(),
                                    ));
                                    this.toast(
                                        Tone::Positive,
                                        copy::ux_for(this.workspace.locale()).toast_copied,
                                        None,
                                        cx,
                                    );
                                })),
                        );
                    }
                    let edit_sel = format!("security.key.edit.{index}");
                    let edit_btn = components::icon_only_button(Icon::Pencil, metrics)
                        .id(SharedString::from(edit_sel.clone()))
                        .debug_selector({
                            let selector = edit_sel.clone();
                            move || selector.clone()
                        })
                        .tooltip(components::tooltip(strings.edit));
                    row_actions = row_actions.child(if busy {
                        edit_btn
                    } else {
                        edit_btn.on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.security_start(SecurityAction::EditKey(key_id), cx);
                        }))
                    });
                }
                SecurityRecord::Credential(cred_id) => {
                    if let Some(user_val) = user.filter(|u| !u.trim().is_empty()) {
                        let copy_user_sel = format!("security.credential.copy-user.{index}");
                        row_actions = row_actions.child(
                            components::icon_only_button(Icon::Copy, metrics)
                                .id(SharedString::from(copy_user_sel.clone()))
                                .debug_selector({
                                    let selector = copy_user_sel.clone();
                                    move || selector.clone()
                                })
                                .tooltip(components::tooltip(strings.copy_username))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    cx.stop_propagation();
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        user_val.clone(),
                                    ));
                                    this.toast(
                                        Tone::Positive,
                                        copy::ux_for(this.workspace.locale()).toast_copied,
                                        Some(user_val.clone()),
                                        cx,
                                    );
                                })),
                        );
                    }
                    let copy_pass_sel = format!("security.credential.copy-password.{index}");
                    row_actions = row_actions.child(
                        components::icon_only_button(Icon::Lock, metrics)
                            .id(SharedString::from(copy_pass_sel.clone()))
                            .debug_selector({
                                let selector = copy_pass_sel.clone();
                                move || selector.clone()
                            })
                            .tooltip(components::tooltip(strings.copy_password))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                cx.stop_propagation();
                                this.security_copy_password(cred_id, cx);
                            })),
                    );
                    let edit_sel = format!("security.credential.edit.{index}");
                    let edit_btn = components::icon_only_button(Icon::Pencil, metrics)
                        .id(SharedString::from(edit_sel.clone()))
                        .debug_selector({
                            let selector = edit_sel.clone();
                            move || selector.clone()
                        })
                        .tooltip(components::tooltip(strings.edit));
                    row_actions = row_actions.child(if busy {
                        edit_btn
                    } else {
                        edit_btn.on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.security_start(SecurityAction::EditCredential(cred_id), cx);
                        }))
                    });
                }
            }

            let hover_bg = theme::swatch(palette::SURFACE_HOVER);
            card = card.child(
                div()
                    .group("security-row")
                    .flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .when(index + 1 < count, |row| {
                        row.border_b_1()
                            .border_color(theme::swatch(palette::BORDER))
                    })
                    .when(chosen, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .when(!chosen && focused, |row| row.bg(hover_bg))
                    .when(!chosen, |row| row.hover(move |style| style.bg(hover_bg)))
                    .cursor_pointer()
                    .id(SharedString::from(row_selector.clone()))
                    .debug_selector({
                        let selector = row_selector.clone();
                        move || selector.clone()
                    })
                    .child(
                        components::accent_mark(chosen || focused, metrics)
                            .h_auto()
                            .my(metrics.scale(spacing::SNUG)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(metrics.scale(spacing::HAIRLINE))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(metrics.scale(spacing::TIGHT))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(metrics.body_size())
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                            .child(security_highlighted(&name, &query)),
                                    )
                                    .child(components::chip_within(
                                        SharedString::from(chip),
                                        Tone::Accent,
                                        metrics,
                                    )),
                            )
                            .when(!caption.is_empty(), |col| {
                                col.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(metrics.caption_size())
                                        .text_color(theme::swatch(palette::TEXT_MUTED))
                                        .child(security_highlighted(&caption, &query)),
                                )
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(security_highlighted(&meta_line, &query)),
                            ),
                    )
                    .child(row_actions)
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.settings_nav.clear_nav_focus();
                        window.focus(&this.security.list_focus);
                        this.security_pick(record, cx);
                    })),
            );
        }
        column.child(card)
    }

    /// The rows of one list, narrowed by the search and active [`SecurityFilter`].
    fn security_rows(&self, keys: bool, cx: &App) -> Vec<SecurityRow> {
        let Some(content) = self.security.content.as_ref() else {
            return Vec::new();
        };
        let strings = self.security_strings();
        let unused_only = self.security.filter == SecurityFilter::Unused;
        let query = self.security.search.read(cx).text().trim().to_lowercase();
        let needles: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
        let narrowed = !needles.is_empty();
        let matches = |haystacks: &[String]| -> bool {
            !narrowed
                || needles
                    .iter()
                    .all(|needle| haystacks.iter().any(|text| text.contains(needle)))
        };
        let mut rows = Vec::new();
        if keys {
            // Rows follow their names, not the vault's internal order: a
            // reader finds a key where the alphabet put it, and a test can name
            // the row it means. Records that share a name — an independent copy
            // starts out named like its original — keep the older one first, so
            // the list a reader sees does not reshuffle between runs.
            let mut live: Vec<_> = content
                .keys()
                .filter(|record| record.meta().is_live())
                .collect();
            live.sort_by(|a, b| {
                a.meta()
                    .name()
                    .cmp(b.meta().name())
                    .then_with(|| a.meta().created_at().cmp(&b.meta().created_at()))
                    .then_with(|| a.meta().id().cmp(&b.meta().id()))
            });
            for entry in live {
                let meta = entry.meta();
                if !meta.is_live() {
                    continue;
                }
                let referencing = self.referencing_records(Some(meta.id()), None);
                if unused_only && !referencing.is_empty() {
                    continue;
                }
                let haystacks: Vec<String> = [
                    meta.name().to_lowercase(),
                    meta.algorithm().to_lowercase(),
                    meta.fingerprint().to_lowercase(),
                    meta.comment().unwrap_or_default().to_lowercase(),
                ]
                .into_iter()
                .chain(referencing.iter().map(|host| host.name().to_lowercase()))
                .collect();
                if !matches(&haystacks) {
                    continue;
                }
                rows.push(SecurityRow {
                    record: SecurityRecord::Key(meta.id()),
                    name: meta.name().to_owned(),
                    caption: format!("{} · {}", meta.algorithm(), meta.fingerprint()),
                    chip: strings.kind_key.to_owned(),
                    hosts: referencing.len(),
                    host_names: referencing
                        .iter()
                        .map(|host| host.name().to_owned())
                        .collect(),
                    public_key: Some(meta.public_key().to_owned()),
                    user: None,
                    created: meta.created_at(),
                });
            }
        } else {
            let mut live: Vec<_> = content
                .credentials()
                .filter(|record| record.meta().is_live())
                .collect();
            live.sort_by(|a, b| {
                a.meta()
                    .name()
                    .cmp(b.meta().name())
                    .then_with(|| a.meta().created_at().cmp(&b.meta().created_at()))
                    .then_with(|| a.meta().id().cmp(&b.meta().id()))
            });
            for entry in live {
                let meta = entry.meta();
                let referencing = self.referencing_records(None, Some(meta.id()));
                if unused_only && !referencing.is_empty() {
                    continue;
                }
                let haystacks: Vec<String> = [
                    meta.name().to_lowercase(),
                    meta.user().unwrap_or_default().to_lowercase(),
                ]
                .into_iter()
                .chain(referencing.iter().map(|host| host.name().to_lowercase()))
                .collect();
                if !matches(&haystacks) {
                    continue;
                }
                rows.push(SecurityRow {
                    record: SecurityRecord::Credential(meta.id()),
                    name: meta.name().to_owned(),
                    caption: meta.user().map(str::to_owned).unwrap_or_default(),
                    chip: strings.kind_credential.to_owned(),
                    hosts: referencing.len(),
                    host_names: referencing
                        .iter()
                        .map(|host| host.name().to_owned())
                        .collect(),
                    public_key: None,
                    user: meta.user().map(str::to_owned),
                    created: meta.created_at(),
                });
            }
        }
        rows
    }

    /// The connections that reference one record of the vault, whatever their
    /// protocol: an SSH host and a desktop record name a record the same way,
    /// so the page counts and lists them together (R1).
    fn referencing_records(
        &self,
        key: Option<KeyId>,
        credential: Option<CredentialId>,
    ) -> Vec<ConnectionRecord> {
        self.book
            .records()
            .into_iter()
            .filter(|record| match (key, credential) {
                (Some(key), _) => {
                    matches!(record, ConnectionRecord::Ssh(host) if host.managed_key_id() == Some(key))
                }
                (_, Some(credential)) => record.credential_id() == Some(credential),
                _ => false,
            })
            .collect()
    }

    /// The detail card of one record: its public facts, its references, and the
    /// actions its life cycle offers.
    fn security_detail(&self, record: SecurityRecord, cx: &mut Context<Self>) -> Option<Div> {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let content = self.security.content.as_ref()?;
        let busy = self.security.busy() || self.security.dialog.is_some();
        let mut card = components::card(metrics)
            .w_full()
            .p(px(0.0))
            .gap(px(0.0))
            .debug_selector(|| "security.detail".to_owned());

        let close_button = components::icon_only_button(Icon::Close, metrics)
            .id("security.detail.close")
            .debug_selector(|| "security.detail.close".to_owned())
            .tooltip(components::tooltip(strings.close))
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.security_close_detail(cx);
            }));

        match record {
            SecurityRecord::Key(id) => {
                let entry = content.key(id)?;
                let meta = entry.meta();
                let referencing = self.referencing_records(Some(id), None);
                let rotate_disabled = referencing
                    .is_empty()
                    .then_some(strings.rotate_disabled_unused);
                let mut body = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w_0()
                    .gap(metrics.scale(spacing::TIGHT))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "security.detail.key".to_owned())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(metrics.scale(spacing::TIGHT))
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(metrics.scale(spacing::TIGHT))
                                    .min_w_0()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(metrics.body_size())
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                            .child(SharedString::from(meta.name().to_owned())),
                                    )
                                    .child(components::chip_within(
                                        SharedString::from(strings.kind_key),
                                        Tone::Accent,
                                        metrics,
                                    )),
                            )
                            .child(close_button),
                    );
                if let Some(comment) = meta.comment().filter(|text| !text.trim().is_empty()) {
                    body = body.child(self.security_fact(
                        strings.label_comment,
                        comment.to_owned(),
                        None,
                        None,
                        cx,
                    ));
                }
                body = body
                    .child(self.security_fact(
                        strings.label_algorithm,
                        meta.algorithm().to_owned(),
                        None,
                        None,
                        cx,
                    ))
                    .child(self.security_fact(
                        strings.label_fingerprint,
                        meta.fingerprint().to_owned(),
                        Some("security.detail.fingerprint"),
                        Some(("security.detail.copy.fingerprint", strings.copy_fingerprint)),
                        cx,
                    ))
                    .child(self.security_fact(
                        strings.label_public_key,
                        meta.public_key().to_owned(),
                        Some("security.detail.public"),
                        Some(("security.detail.copy.public", strings.copy_public_key)),
                        cx,
                    ));
                card = card.child(body);
                card = card.child(self.security_references(&referencing, record, cx));
                card = card.child(self.security_actions_row(
                    &[
                        (SecurityAction::EditKey(id), strings.edit, None),
                        (
                            SecurityAction::RotateKey(id),
                            strings.rotate,
                            rotate_disabled,
                        ),
                        (SecurityAction::ExportKey(id), strings.export_choose, None),
                        (SecurityAction::DeleteKey(id), strings.delete, None),
                    ],
                    busy,
                    cx,
                ));
            }
            SecurityRecord::Credential(id) => {
                let entry = content.credential(id)?;
                let meta = entry.meta();
                let referencing = self.referencing_records(None, Some(id));
                let mut body = div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w_0()
                    .gap(metrics.scale(spacing::TIGHT))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .debug_selector(|| "security.detail.credential".to_owned())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(metrics.scale(spacing::TIGHT))
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(metrics.scale(spacing::TIGHT))
                                    .min_w_0()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(metrics.body_size())
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                                            .child(SharedString::from(meta.name().to_owned())),
                                    )
                                    .child(components::chip_within(
                                        SharedString::from(strings.kind_credential),
                                        Tone::Accent,
                                        metrics,
                                    )),
                            )
                            .child(close_button),
                    );
                if let Some(user) = meta.user().filter(|text| !text.trim().is_empty()) {
                    // Copyable the way the fingerprint and the public key are:
                    // a login user name is what a person pastes into the next
                    // form (R1).
                    body = body.child(self.security_fact(
                        strings.label_user,
                        user.to_owned(),
                        Some("security.detail.user"),
                        Some(("security.detail.copy.user", strings.copy_username)),
                        cx,
                    ));
                }
                // The password itself: hidden by default, shown while this one
                // card asks for it, and copyable without ever being drawn.
                let shown = self.security.revealed;
                let value = entry.secret().map(|secret| secret.expose().to_owned());
                let mut password_row = div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .flex_wrap()
                    .min_w_0();
                password_row = password_row
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.label_password),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "security.detail.password".to_owned())
                            .child(SharedString::from(match (&value, shown) {
                                (Some(text), true) => text.clone(),
                                (Some(_), false) => "••••••••".to_owned(),
                                (None, _) => String::new(),
                            })),
                    );
                if value.is_some() {
                    if shown {
                        password_row = password_row.child(
                            components::compact_button(
                                if shown { strings.hide } else { strings.show },
                                Variant::Ghost,
                                metrics,
                            )
                            .id("security.detail.reveal")
                            .debug_selector(|| "security.detail.reveal".to_owned())
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    this.security.revealed = false;
                                    cx.notify();
                                },
                            )),
                        );
                    } else {
                        let reveal_id = id;
                        password_row = password_row.child(
                            components::compact_button(strings.show, Variant::Ghost, metrics)
                                .id("security.detail.reveal")
                                .debug_selector(|| "security.detail.reveal".to_owned())
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.security_reveal(reveal_id, cx);
                                })),
                        );
                    }
                    // Copy goes through the same gate as showing: the policy
                    // that verifies every sensitive operation counts a copy as
                    // one, and a pass is never carried to the next (R3).
                    let copy_id = id;
                    password_row = password_row.child(
                        components::compact_button(strings.copy_value, Variant::Ghost, metrics)
                            .id("security.detail.copy.password")
                            .debug_selector(|| "security.detail.copy.password".to_owned())
                            .tooltip(components::tooltip(strings.copy_password))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.security_copy_password(copy_id, cx);
                            })),
                    );
                }
                body = body.child(password_row);
                card = card.child(body);
                card = card.child(self.security_references(&referencing, record, cx));
                card = card.child(self.security_actions_row(
                    &[
                        (SecurityAction::EditCredential(id), strings.edit, None),
                        (
                            SecurityAction::DuplicateCredential(id),
                            strings.duplicate,
                            None,
                        ),
                        (SecurityAction::DeleteCredential(id), strings.delete, None),
                    ],
                    busy,
                    cx,
                ));
            }
        }
        Some(card)
    }

    /// One fact line of a detail card, with an explicit copy button and toast when copyable.
    fn security_fact(
        &self,
        label: &'static str,
        value: String,
        selector: Option<&'static str>,
        copy_control: Option<(&'static str, &'static str)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE));
        column = column.child(
            div()
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(label),
        );
        let mut line = div()
            .min_w_0()
            .max_w(metrics.scale(PUBLIC_KEY_MAX_WIDTH))
            .truncate()
            .font_family(crate::app::terminal::monospace_family())
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY));
        if let Some(selector) = selector {
            line = line.debug_selector(move || selector.to_owned());
        }
        let copy_value = value.clone();
        let line = if selector.is_some() {
            components::copy_on_click(line, value.clone()).child(SharedString::from(value))
        } else {
            line.child(SharedString::from(value))
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .flex_wrap()
            .min_w_0()
            .child(line);
        if let Some((copy_selector, copy_tip)) = copy_control {
            row = row.child(
                components::compact_button(strings.copy_value, Variant::Ghost, metrics)
                    .id(SharedString::from(copy_selector))
                    .debug_selector(move || copy_selector.to_owned())
                    .tooltip(components::tooltip(copy_tip))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_value.clone()));
                        this.toast(
                            Tone::Positive,
                            copy::ux_for(this.workspace.locale()).toast_copied,
                            None,
                            cx,
                        );
                    })),
            );
        }
        column.child(row)
    }

    /// The connections that reference a record, each with edit and unbind actions.
    fn security_references(
        &self,
        hosts: &[ConnectionRecord],
        record: SecurityRecord,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let busy = self.security.busy() || self.security.dialog.is_some();
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(|| "security.detail.references".to_owned())
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(strings.referenced_by),
            );
        if hosts.is_empty() {
            return column.child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(strings.not_referenced),
            );
        }
        for (index, host) in hosts.iter().enumerate() {
            let id = host.id();
            let edit_selector = format!("security.detail.edit-host.{index}");
            let selector = format!("security.detail.unbind.{index}");
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w_0()
                    .flex_wrap()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics.label_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(host.name().to_owned())),
                    )
                    .child(
                        components::compact_button(strings.edit, Variant::Ghost, metrics)
                            .id(SharedString::from(edit_selector.clone()))
                            .debug_selector({
                                let edit_selector = edit_selector.clone();
                                move || edit_selector.clone()
                            })
                            .when(!busy, |button| {
                                button.on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.edit_saved_connection(id, cx);
                                }))
                            }),
                    )
                    .child(
                        components::compact_button(strings.unbind, Variant::Ghost, metrics)
                            .id(SharedString::from(selector.clone()))
                            .debug_selector({
                                let selector = selector.clone();
                                move || selector.clone()
                            })
                            .when(!busy, |button| {
                                button.on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.security_unbind(record, id, cx);
                                }))
                            }),
                    ),
            );
        }
        column
    }

    /// The action buttons of one detail card.
    fn security_actions_row(
        &self,
        actions: &[(SecurityAction, &'static str, Option<&'static str>)],
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::BASE))
            .py(metrics.scale(spacing::SNUG))
            .debug_selector(|| "security.detail.actions".to_owned());
        for (index, (action, label, disabled_reason)) in actions.iter().copied().enumerate() {
            let selector = format!("security.detail.action.{index}");
            let reserved = components::compact_label_reserved_width(label, metrics);
            let state = if disabled_reason.is_some() || busy {
                ControlState::Disabled
            } else {
                ControlState::Resting
            };
            let button =
                components::compact_button_with_state(label, Variant::Secondary, state, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    });
            let button = if let Some(reason) = disabled_reason {
                button.tooltip(components::tooltip(reason))
            } else {
                button
            };
            row = row.child(if busy || disabled_reason.is_some() {
                button
            } else {
                button.on_click(cx.listener(move |this, _event, _window, cx| {
                    this.security_start(action, cx);
                }))
            });
        }
        row
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /// Opens the dialog, or runs the immediate action, one control asked for.
    pub(crate) fn security_start(&mut self, action: SecurityAction, cx: &mut Context<Self>) {
        if self.security.busy() || self.security.dialog.is_some() {
            return;
        }
        let strings = self.security_strings();
        match action {
            SecurityAction::Import => {
                let pem = self.security_dialog_text_field(strings.import_pem, "", true, false, cx);
                let passphrase = self.security_secret_field(cx);
                let name =
                    self.security_dialog_text_field(strings.import_name, "", false, true, cx);
                self.field_focus_pending = Some(pem.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::ImportKey {
                    pem,
                    passphrase,
                    name,
                    refusal: None,
                });
            }
            SecurityAction::Generate => {
                let name =
                    self.security_dialog_text_field(strings.generate_name, "", false, true, cx);
                let comment =
                    self.security_dialog_text_field(strings.generate_comment, "", false, true, cx);
                self.field_focus_pending = Some(name.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::GenerateKey {
                    kind: keys_api::ManagedKeyKind::Ed25519,
                    name,
                    comment,
                    refusal: None,
                });
            }
            SecurityAction::NewCredential => {
                let name =
                    self.security_dialog_text_field(strings.credential_name, "", false, true, cx);
                let user =
                    self.security_dialog_text_field(strings.credential_user, "", false, true, cx);
                let secret = self.security_secret_field(cx);
                let length = self.security_dialog_text_field(
                    "",
                    &PASSWORD_LENGTH_DEFAULT.to_string(),
                    false,
                    false,
                    cx,
                );
                self.field_focus_pending = Some(name.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::NewCredential {
                    name,
                    user,
                    secret,
                    length,
                    letters: true,
                    digits: true,
                    symbols: true,
                    refusal: None,
                });
            }
            SecurityAction::EditKey(id) => {
                let Some((initial_name, initial_comment)) = self
                    .security
                    .content
                    .as_ref()
                    .and_then(|vault| vault.key(id))
                    .map(|entry| {
                        (
                            entry.meta().name().to_owned(),
                            entry.meta().comment().unwrap_or_default().to_owned(),
                        )
                    })
                else {
                    return;
                };
                let name = self.security_dialog_text_field("", &initial_name, false, true, cx);
                let comment =
                    self.security_dialog_text_field("", &initial_comment, false, true, cx);
                self.field_focus_pending = Some(name.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::EditKey {
                    key: id,
                    name,
                    comment,
                    refusal: None,
                });
            }
            SecurityAction::ExportKey(id) => {
                if self.security_key_missing(id) {
                    return;
                }
                let passphrase = self.security_secret_field(cx);
                let confirm = self.security_secret_field(cx);
                self.field_focus_pending = Some(passphrase.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::ExportKey {
                    key: id,
                    passphrase,
                    confirm,
                    plain: false,
                    plain_confirmed: false,
                    choosing: false,
                    refusal: None,
                    saved_to: None,
                });
            }
            SecurityAction::RotateKey(id) => {
                let hosts: Vec<(HostId, bool)> = self
                    .referencing_records(Some(id), None)
                    .iter()
                    .map(|host| (host.id(), true))
                    .collect();
                if hosts.is_empty() {
                    return;
                }
                let previous_name = self
                    .security
                    .content
                    .as_ref()
                    .and_then(|vault| vault.key(id))
                    .map(|entry| entry.meta().name().to_owned())
                    .unwrap_or_default();
                let name = self.security_dialog_text_field(
                    "",
                    &format!("{previous_name} 2"),
                    false,
                    true,
                    cx,
                );
                self.field_focus_pending = Some(name.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::RotateKey {
                    previous: id,
                    kind: keys_api::ManagedKeyKind::Ed25519,
                    name,
                    hosts,
                    retire: false,
                    refusal: None,
                });
            }
            SecurityAction::DeleteKey(id) => {
                let affected = self
                    .referencing_records(Some(id), None)
                    .iter()
                    .map(ConnectionRecord::id)
                    .collect();
                self.security.dialog = Some(SecurityDialog::DeleteKey { key: id, affected });
            }
            SecurityAction::EditCredential(id) => {
                let Some((initial_name, initial_user)) = self
                    .security
                    .content
                    .as_ref()
                    .and_then(|vault| vault.credential(id))
                    .map(|entry| {
                        (
                            entry.meta().name().to_owned(),
                            entry.meta().user().unwrap_or_default().to_owned(),
                        )
                    })
                else {
                    return;
                };
                let name = self.security_dialog_text_field("", &initial_name, false, true, cx);
                let user = self.security_dialog_text_field("", &initial_user, false, true, cx);
                let secret = self.security_secret_field(cx);
                let length = self.security_dialog_text_field(
                    "",
                    &PASSWORD_LENGTH_DEFAULT.to_string(),
                    false,
                    false,
                    cx,
                );
                self.field_focus_pending = Some(name.read(cx).focus_handle().clone());
                self.security.dialog = Some(SecurityDialog::EditCredential {
                    credential: id,
                    name,
                    user,
                    secret,
                    length,
                    letters: true,
                    digits: true,
                    symbols: true,
                    refusal: None,
                });
            }
            SecurityAction::DuplicateCredential(id) => self.security_duplicate(id, cx),
            SecurityAction::DeleteCredential(id) => {
                let affected = self
                    .referencing_records(None, Some(id))
                    .iter()
                    .map(ConnectionRecord::id)
                    .collect();
                self.security.dialog = Some(SecurityDialog::DeleteCredential {
                    credential: id,
                    affected,
                });
            }
        }
        cx.notify();
    }

    /// Creates a dialog text field that clears stale refusal banners on edit and
    /// optionally submits the dialog on Enter.
    fn security_dialog_text_field(
        &self,
        placeholder: &'static str,
        initial: &str,
        keeps_breaks: bool,
        submit_on_enter: bool,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        let initial_owned = initial.to_owned();
        let field = cx.new(|cx| {
            let mut field = if initial_owned.is_empty() {
                TextField::new(placeholder, cx)
            } else {
                TextField::with_text(placeholder, initial_owned, cx)
            };
            if keeps_breaks {
                field.set_keeps_line_breaks(true, cx);
            }
            field
        });
        if submit_on_enter {
            let view = cx.entity().downgrade();
            field.update(cx, |field, _cx| {
                field.on_submit(move |_text, window, app| {
                    let submit_view = view.clone();
                    window.defer(app, move |_window, app| {
                        let _ = submit_view.update(app, |view, cx| {
                            view.security_submit(cx);
                        });
                    });
                    false
                });
            });
        }
        self.security_observe_dialog_field(&field, cx);
        field
    }

    /// A masked, revealable field for a secret a dialog asks for.
    fn security_secret_field(&self, cx: &mut Context<Self>) -> Entity<TextField> {
        let strings = self.security_strings();
        let field = cx.new(|cx| {
            let mut field = TextField::new(strings.credential_password, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        let view = cx.entity().downgrade();
        field.update(cx, |field, _cx| {
            field.on_submit(move |_text, window, app| {
                let submit_view = view.clone();
                window.defer(app, move |_window, app| {
                    let _ = submit_view.update(app, |view, cx| {
                        view.security_submit(cx);
                    });
                });
                false
            });
        });
        self.security_observe_dialog_field(&field, cx);
        field
    }

    fn security_observe_dialog_field(&self, field: &Entity<TextField>, cx: &mut Context<Self>) {
        let mut previous = field.read(cx).text().to_owned();
        cx.observe(field, move |this, field, cx| {
            let now = field.read(cx).text();
            if now != previous {
                previous = now.to_owned();
                if let Some(slot) = this.security_refusal_slot() {
                    *slot = None;
                }
            }
            cx.notify();
        })
        .detach();
    }

    /// Opens the system file picker to load a local private key file into the Import dialog.
    fn security_browse_import_file(&mut self, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |view, cx| view.security_load_import_file(&path, cx));
        })
        .detach();
    }

    /// Reads `path` into the open `ImportKey` dialog's PEM field and auto-fills `name`
    /// with the filename when `name` is still empty.
    pub(crate) fn security_load_import_file(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        let strings = self.security_strings();
        let Some(SecurityDialog::ImportKey {
            pem, name, refusal, ..
        }) = self.security.dialog.as_mut()
        else {
            return;
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let pem = pem.clone();
                let name = name.clone();
                *refusal = None;
                pem.update(cx, |field, cx| field.set_text(contents, cx));
                if name.read(cx).text().trim().is_empty()
                    && let Some(file_name) = path
                        .file_name()
                        .and_then(|os| os.to_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                {
                    let suggested = file_name.to_owned();
                    name.update(cx, |field, cx| field.set_text(suggested, cx));
                }
            }
            Err(error) => {
                *refusal = Some(format!("{}: {error}", strings.import_file_failed));
            }
        }
        cx.notify();
    }

    fn security_key_missing(&self, id: KeyId) -> bool {
        self.security
            .content
            .as_ref()
            .and_then(|vault| vault.key(id))
            .is_none()
    }

    /// Switches the active [`SecurityFilter`], clearing revealed secrets and any
    /// detail selection that the new filter hides.
    fn security_set_filter(&mut self, filter: SecurityFilter, cx: &mut Context<Self>) {
        if self.security.filter == filter {
            return;
        }
        self.security.filter = filter;
        self.security.revealed = false;
        let visible = self.security_visible_records(cx);
        if let Some(detail) = self.security.detail
            && !visible.contains(&detail)
        {
            self.security.detail = None;
        }
        if let Some(cursor) = self.security.cursor
            && !visible.contains(&cursor)
        {
            self.security.cursor = visible.first().copied();
        }
        cx.notify();
    }

    /// All currently visible records in visual list order.
    pub(crate) fn security_visible_records(&self, cx: &App) -> Vec<SecurityRecord> {
        if self.security.lock.locked {
            return Vec::new();
        }
        let mut out = Vec::new();
        match self.security.filter {
            SecurityFilter::All | SecurityFilter::Unused => {
                out.extend(self.security_rows(true, cx).into_iter().map(|r| r.record));
                out.extend(self.security_rows(false, cx).into_iter().map(|r| r.record));
            }
            SecurityFilter::Keys => {
                out.extend(self.security_rows(true, cx).into_iter().map(|r| r.record));
            }
            SecurityFilter::Credentials => {
                out.extend(self.security_rows(false, cx).into_iter().map(|r| r.record));
            }
        }
        out
    }

    /// Moves keyboard focus from the search box into the vault record list.
    pub(crate) fn security_focus_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let visible = self.security_visible_records(cx);
        if visible.is_empty() {
            return false;
        }
        if self.security.cursor.is_none_or(|c| !visible.contains(&c)) {
            self.security.cursor = self
                .security
                .detail
                .filter(|d| visible.contains(d))
                .or_else(|| visible.first().copied());
        }
        window.focus(&self.security.list_focus);
        cx.notify();
        true
    }

    /// Steps the keyboard cursor up or down across visible vault records.
    pub(crate) fn security_step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let visible = self.security_visible_records(cx);
        if visible.is_empty() {
            return false;
        }
        let current = self
            .security
            .cursor
            .or(self.security.detail)
            .and_then(|record| visible.iter().position(|candidate| *candidate == record));
        let next = match current {
            Some(index) => {
                if delta >= 0 {
                    (index + 1).min(visible.len() - 1)
                } else {
                    index.saturating_sub(1)
                }
            }
            None => {
                if delta >= 0 {
                    0
                } else {
                    visible.len() - 1
                }
            }
        };
        self.security.cursor = Some(visible[next]);
        cx.notify();
        true
    }

    /// Opens the detail card for the keyboard-focused vault record.
    pub(crate) fn security_activate_cursor(&mut self, cx: &mut Context<Self>) -> bool {
        let visible = self.security_visible_records(cx);
        if visible.is_empty() {
            return false;
        }
        let target = self
            .security
            .cursor
            .filter(|record| visible.contains(record))
            .or_else(|| visible.first().copied());
        let Some(record) = target else {
            return false;
        };
        self.security.cursor = Some(record);
        self.security_pick(record, cx);
        true
    }

    fn security_workbench_capture_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.security.lock.locked || self.security.dialog.is_some() {
            return false;
        }
        if event.keystroke.modifiers.number_of_modifiers() != 0 {
            return false;
        }
        let search_focused = self
            .security
            .search
            .read(cx)
            .focus_handle()
            .is_focused(window);
        let list_focused = self.security.list_focus.is_focused(window);
        match event.keystroke.key.as_str() {
            "down" => {
                if search_focused {
                    self.settings_nav.clear_nav_focus();
                    window.focus(&self.security.list_focus);
                }
                self.security_step_cursor(1, cx) || list_focused
            }
            "up" if !search_focused => self.security_step_cursor(-1, cx) || list_focused,
            "enter" => {
                let activated = self.security_activate_cursor(cx);
                if activated && search_focused {
                    self.settings_nav.clear_nav_focus();
                    window.focus(&self.security.list_focus);
                }
                activated
            }
            _ => false,
        }
    }

    /// Chooses the record whose detail card is drawn.
    fn security_pick(&mut self, record: SecurityRecord, cx: &mut Context<Self>) {
        if self.workspace.destination() == super::Destination::Settings {
            self.settings_note_keys_item();
        }
        self.security.detail = Some(record);
        self.security.cursor = Some(record);
        // A reveal belongs to the record it was asked for, never to the next.
        self.security.revealed = false;
        cx.notify();
    }

    /// Collapses the detail card and clears any revealed secret.
    pub(crate) fn security_close_detail(&mut self, cx: &mut Context<Self>) -> bool {
        if self.security.detail.is_some() || self.security.revealed {
            self.security.detail = None;
            self.security.revealed = false;
            cx.notify();
            return true;
        }
        false
    }

    /// Whether a shared credential secret is currently revealed in the detail card.
    pub(crate) fn security_is_revealed(&self) -> bool {
        self.security.revealed
    }

    /// Resets any revealed secret when leaving the vault view.
    pub(crate) fn security_reset_reveal(&mut self) {
        self.security.revealed = false;
    }

    /// Closes the page's dialog; a job in flight keeps it until it ends.
    pub(crate) fn security_close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.security.busy {
            return;
        }
        let had_bind = self.security.pending_bind.take();
        self.security.dialog = None;
        self.security.rotation_public_key = None;
        let return_to_connection_dialog = match had_bind {
            Some(PendingVaultBind::SshManagedKey | PendingVaultBind::SshSharedCredential) => {
                self.host_fields.is_some()
            }
            Some(PendingVaultBind::DesktopSharedCredential) => self.protocol_fields.is_some(),
            None => false,
        };
        if return_to_connection_dialog {
            self.focus_host_dialog = true;
            self.focus_host_dialog_keep_return = true;
        } else {
            if self.host_fields.is_none() && self.protocol_fields.is_none() && self.backup.is_none()
            {
                self.modal_focus_captured = false;
            }
            self.restore_focus_pending = true;
        }
        cx.notify();
    }

    /// Changes the open dialog, when there is one to change.
    fn security_dialog_with<R>(
        &mut self,
        change: impl FnOnce(&mut SecurityDialog) -> R,
        cx: &mut Context<Self>,
    ) -> Option<R> {
        let result = self.security.dialog.as_mut().map(change);
        if result.is_some() {
            cx.notify();
        }
        result
    }

    /// Chooses the kind a generation or a rotation generates.
    fn security_set_generate_kind(
        &mut self,
        kind: keys_api::ManagedKeyKind,
        cx: &mut Context<Self>,
    ) {
        self.security_dialog_with(
            |dialog| match dialog {
                SecurityDialog::GenerateKey { kind: slot, .. }
                | SecurityDialog::RotateKey { kind: slot, .. } => *slot = kind,
                _ => {}
            },
            cx,
        );
    }

    /// Turns the plain export of a private key on or off.
    fn security_toggle_plain_export(&mut self, cx: &mut Context<Self>) {
        self.security_dialog_with(
            |dialog| {
                if let SecurityDialog::ExportKey {
                    plain,
                    plain_confirmed,
                    refusal,
                    ..
                } = dialog
                {
                    *plain = !*plain;
                    *plain_confirmed = false;
                    *refusal = None;
                }
            },
            cx,
        );
    }

    /// Accepts, or takes back, the risk of a plain export.
    fn security_toggle_plain_confirm(&mut self, cx: &mut Context<Self>) {
        self.security_dialog_with(
            |dialog| {
                if let SecurityDialog::ExportKey {
                    plain_confirmed, ..
                } = dialog
                {
                    *plain_confirmed = !*plain_confirmed;
                }
            },
            cx,
        );
    }

    /// Chooses or unchooses one connection of a rotation.
    fn security_toggle_rotate_host(&mut self, host: HostId, cx: &mut Context<Self>) {
        self.security_dialog_with(
            |dialog| {
                if let SecurityDialog::RotateKey { hosts, .. } = dialog {
                    for (candidate, chosen) in hosts.iter_mut() {
                        if *candidate == host {
                            *chosen = !*chosen;
                        }
                    }
                }
            },
            cx,
        );
    }

    /// Chooses whether a rotation deletes the old key once nothing uses it.
    fn security_toggle_retire(&mut self, cx: &mut Context<Self>) {
        self.security_dialog_with(
            |dialog| {
                if let SecurityDialog::RotateKey { retire, .. } = dialog {
                    *retire = !*retire;
                }
            },
            cx,
        );
    }

    /// Turns one character class of the password generator on or off.
    fn security_toggle_charset(&mut self, which: u8, cx: &mut Context<Self>) {
        self.security_dialog_with(
            |dialog| match dialog {
                SecurityDialog::NewCredential {
                    letters,
                    digits,
                    symbols,
                    ..
                }
                | SecurityDialog::EditCredential {
                    letters,
                    digits,
                    symbols,
                    ..
                } => match which {
                    0 => *letters = !*letters,
                    1 => *digits = !*digits,
                    _ => *symbols = !*symbols,
                },
                _ => {}
            },
            cx,
        );
    }

    /// Fills the password field with a generated value, so the value is
    /// visible, editable and refused as a whole when no class remains.
    fn security_generate_password(&mut self, cx: &mut Context<Self>) {
        let (secret, length, letters, digits, symbols) = match self.security.dialog.as_ref() {
            Some(SecurityDialog::NewCredential {
                secret,
                length,
                letters,
                digits,
                symbols,
                ..
            })
            | Some(SecurityDialog::EditCredential {
                secret,
                length,
                letters,
                digits,
                symbols,
                ..
            }) => (secret.clone(), length.clone(), *letters, *digits, *symbols),
            _ => return,
        };
        let length = length
            .read(cx)
            .text()
            .trim()
            .parse::<usize>()
            .ok()
            .unwrap_or(PASSWORD_LENGTH_DEFAULT)
            .clamp(PASSWORD_LENGTH_MIN, PASSWORD_LENGTH_MAX);
        let value = generate_password(length, letters, digits, symbols);
        secret.update(cx, |field, cx| field.set_text(value, cx));
        cx.notify();
    }

    /// Unbinds one connection from the record its detail card shows: the record
    /// keeps everything else and asks for a sign-in choice at the next
    /// connection.
    fn security_unbind(&mut self, record: SecurityRecord, host: HostId, cx: &mut Context<Self>) {
        if self.security.busy() || self.security.dialog.is_some() {
            return;
        }
        let Some(name) = self
            .book
            .connection(host)
            .map(|host| host.name().to_owned())
        else {
            return;
        };
        self.security_run(
            Box::new(move |content, hosts, connections| {
                // Only the record's reference goes; the vault's records keep
                // everything they had. An SSH host loses the method that read
                // the reference, which clears it (ADR-1); a desktop record has
                // no method, so the reference itself goes. The changed record
                // alone travels back, so the finish replays this one change
                // instead of overwriting the whole book the job started from.
                let changed = if let Some(target) =
                    hosts.iter().find(|candidate| candidate.id() == host)
                {
                    let mut unbound = target.clone();
                    match record {
                        SecurityRecord::Key(id) if unbound.managed_key_id() == Some(id) => {
                            unbound.set_auth(None);
                        }
                        SecurityRecord::Credential(id) if unbound.credential_id() == Some(id) => {
                            unbound.set_auth(None);
                        }
                        _ => return Err("the connection does not use this record".to_owned()),
                    }
                    ConnectionRecord::Ssh(unbound)
                } else if let Some(target) =
                    connections.iter().find(|candidate| candidate.id() == host)
                {
                    let mut unbound = target.clone();
                    match (&mut unbound, record) {
                        (ConnectionRecord::Rdp(config), SecurityRecord::Credential(id))
                            if config.credential_id() == Some(id) =>
                        {
                            config.set_credential_id(None);
                        }
                        (ConnectionRecord::Vnc(config), SecurityRecord::Credential(id))
                            if config.credential_id() == Some(id) =>
                        {
                            config.set_credential_id(None);
                        }
                        _ => return Err("the connection does not use this record".to_owned()),
                    }
                    unbound
                } else {
                    return Err("the connection is no longer saved".to_owned());
                };
                let _ = content;
                Ok(SecurityChange::Save {
                    report: SecurityReport::Unbound { host: name },
                    records: vec![changed],
                })
            }),
            cx,
        );
    }

    /// Creates an independent copy of a shared password: the same value under
    /// a new identity, so the original's hosts keep the original's future.
    fn security_duplicate(&mut self, id: CredentialId, cx: &mut Context<Self>) {
        if self.security.busy() {
            return;
        }
        let device = self.security.device.clone();
        self.security_run(
            Box::new(move |content, _, _| {
                let record = content
                    .credential(id)
                    .ok_or_else(|| "the vault holds no such password".to_owned())?;
                if record.meta().deleted().is_some() {
                    return Err("the vault holds no such password".to_owned());
                }
                let copy_meta = record.meta().duplicated(&device);
                let secret = record
                    .secret()
                    .cloned()
                    .ok_or_else(|| "the vault holds no such password".to_owned())?;
                content
                    .insert_credential(VaultCredentialRecord::new(copy_meta, secret))
                    .map_err(|error| error.to_string())?;
                Ok(SecurityChange::Save {
                    report: SecurityReport::CredentialDuplicated,
                    records: Vec::new(),
                })
            }),
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Dialog submission
    // -----------------------------------------------------------------------

    /// Runs what the open dialog describes. The dialog's fields are read as
    /// owned values first, so a refusal can be written back into the very
    /// dialog it came from without losing what was typed; the dialog leaves
    /// only when a job was started.
    pub(super) fn security_submit(&mut self, cx: &mut Context<Self>) {
        if self.security.busy() || self.security.dialog.is_none() {
            return;
        }
        let strings = self.security_strings();
        let snapshot = security_dialog_snapshot(self.security.dialog.as_ref(), cx);
        let Some(snapshot) = snapshot else {
            return;
        };
        match snapshot {
            DialogSnapshot::Import {
                pem,
                passphrase,
                name,
            } => {
                if pem.trim().is_empty() || name.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.import_missing.to_owned());
                    }
                    self.security_focus_missing_field(cx);
                    cx.notify();
                    return;
                }
                let device = self.security.device.clone();
                self.security_run(
                    Box::new(move |content, _, _| {
                        security_import(content, &pem, &passphrase, &name, &device, strings)
                    }),
                    cx,
                );
            }
            DialogSnapshot::Generate {
                kind,
                name,
                comment,
            } => {
                if name.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.name_required.to_owned());
                    }
                    self.security_focus_missing_field(cx);
                    cx.notify();
                    return;
                }
                let device = self.security.device.clone();
                self.security_run(
                    Box::new(move |content, _, _| {
                        let key = keys_api::generate(kind).map_err(|e| e.to_string())?;
                        let public =
                            keys_api::public_data(&key).map_err(|error| error.to_string())?;
                        // The vault stores the key as OpenSSH text it can hand
                        // to one attempt at a time; this build never re-encrypts
                        // it with a passphrase of its own.
                        let body = keys_api::export_private_key(&key, None)
                            .map_err(|error| error.to_string())?;
                        let id = KeyId::new();
                        content
                            .insert_key(VaultKeyRecord::new(
                                ManagedKeyMeta::new(
                                    id,
                                    name,
                                    public.algorithm,
                                    public.authorized_key,
                                    public.fingerprint,
                                    (!comment.is_empty()).then(|| comment.clone()),
                                    VaultRecordVersion::new(1, device),
                                ),
                                body,
                            ))
                            .map_err(|error| error.to_string())?;
                        Ok(SecurityChange::Save {
                            report: SecurityReport::KeySaved(id),
                            records: Vec::new(),
                        })
                    }),
                    cx,
                );
            }
            DialogSnapshot::EditKey { key, name, comment } => {
                if name.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.name_required.to_owned());
                    }
                    cx.notify();
                    return;
                }
                self.security_run(
                    Box::new(move |content, _, _| {
                        let record = content
                            .key(key)
                            .ok_or_else(|| "the vault holds no such key".to_owned())?;
                        if record.meta().deleted().is_some() {
                            return Err("the vault holds no such key".to_owned());
                        }
                        let edited = renamed_key_meta(
                            record.meta(),
                            name,
                            if comment.is_empty() {
                                None
                            } else {
                                Some(comment)
                            },
                        );
                        replace_key_meta(content, key, edited)?;
                        Ok(SecurityChange::Save {
                            report: SecurityReport::KeySaved(key),
                            records: Vec::new(),
                        })
                    }),
                    cx,
                );
            }
            DialogSnapshot::Export => {
                // The primary action of the export dialog is the file choice;
                // the job itself starts once a path came back.
                self.security_choose_export(cx);
            }
            DialogSnapshot::Rotate {
                previous,
                kind,
                name,
                moving,
                retire,
            } => {
                if name.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.name_required.to_owned());
                    }
                    cx.notify();
                    return;
                }
                if moving.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.rotate_none.to_owned());
                    }
                    cx.notify();
                    return;
                }
                let device = self.security.device.clone();
                self.security_run(
                    Box::new(move |content, hosts, _connections| {
                        security_rotate(
                            content, hosts, previous, kind, &name, moving, retire, device,
                        )
                    }),
                    cx,
                );
            }
            DialogSnapshot::DeleteKey(key) => {
                // A desktop record never names a key, so the unbind pass only
                // has the hosts to walk.
                self.security_run(
                    Box::new(move |content, hosts, _connections| {
                        security_delete(content, hosts, &[], SecurityRecord::Key(key))
                    }),
                    cx,
                );
            }
            DialogSnapshot::NewCredential {
                name,
                user,
                password,
            } => {
                if name.is_empty() || password.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.password_missing.to_owned());
                    }
                    cx.notify();
                    return;
                }
                let device = self.security.device.clone();
                self.security_run(
                    Box::new(move |content, _, _| {
                        let id = CredentialId::new();
                        content
                            .insert_credential(VaultCredentialRecord::new(
                                CredentialMeta::new(
                                    id,
                                    name,
                                    (!user.is_empty()).then(|| user.clone()),
                                    VaultRecordVersion::new(1, device),
                                ),
                                SecretString::new(password),
                            ))
                            .map_err(|error| error.to_string())?;
                        Ok(SecurityChange::Save {
                            report: SecurityReport::CredentialSaved(id),
                            records: Vec::new(),
                        })
                    }),
                    cx,
                );
            }
            DialogSnapshot::EditCredential {
                credential,
                name,
                user,
                password,
            } => {
                if name.is_empty() {
                    if let Some(slot) = self.security_refusal_slot() {
                        *slot = Some(strings.name_required.to_owned());
                    }
                    cx.notify();
                    return;
                }
                self.security_run(
                    Box::new(move |content, _, _| {
                        let record = content
                            .credential(credential)
                            .ok_or_else(|| "the vault holds no such password".to_owned())?;
                        if record.meta().deleted().is_some() {
                            return Err("the vault holds no such password".to_owned());
                        }
                        let edited = renamed_credential_meta(
                            record.meta(),
                            name,
                            if user.is_empty() { None } else { Some(user) },
                        );
                        // An empty password field keeps the current one, and
                        // the record keeps its identity: that is what makes the
                        // edit reach every host that references it (R1).
                        let secret = if password.is_empty() {
                            record
                                .secret()
                                .cloned()
                                .ok_or_else(|| "the vault holds no such password".to_owned())?
                        } else {
                            SecretString::new(password)
                        };
                        replace_credential(content, credential, edited, secret)?;
                        Ok(SecurityChange::Save {
                            report: SecurityReport::CredentialEdited,
                            records: Vec::new(),
                        })
                    }),
                    cx,
                );
            }
            DialogSnapshot::DeleteCredential(credential) => {
                self.security_run(
                    Box::new(move |content, hosts, connections| {
                        security_delete(
                            content,
                            hosts,
                            connections,
                            SecurityRecord::Credential(credential),
                        )
                    }),
                    cx,
                );
            }
            DialogSnapshot::DeleteTrust { host, port } => {
                self.security_forget_trust(&host, port, cx);
            }
            DialogSnapshot::BiometricRisk => {
                self.security_enable_session_only(cx);
            }
        }
    }

    /// Removes one accepted server identity (R7, ADR-6): from the vault, which
    /// is the authority a sync merges, and from this device's legacy trust
    /// file, so a restart finds nothing left to restore. No password, no
    /// biometric prompt - the local unseal this machine already holds is the
    /// only thing the change ever needs.
    fn security_forget_trust(&mut self, host: &str, port: u16, cx: &mut Context<Self>) {
        let server = format!("{host}:{port}");
        let file_host = host.to_owned();
        let file_port = port;
        let in_vault = self
            .security
            .content
            .as_ref()
            .is_some_and(|vault| vault.trust().entry(host, port).is_some())
            || VaultStore::at(self.store.root()).exists();
        // The local record goes first and synchronously: whatever happens to
        // the vault save behind it, this device's own file no longer restores
        // the identity on the next start.
        if self.trust_book.forget(&file_host, file_port)
            && let Err(error) = self.write_trust_file()
        {
            let reason = error.to_string();
            self.security_set_notice(
                Tone::Danger,
                format!("{}: {reason}", self.security_strings().vault_failed),
                cx,
            );
            cx.notify();
            return;
        }
        self.note_data_changed(cx);
        if in_vault {
            let report_server = server.clone();
            self.security_run(
                Box::new(move |content, _, _| {
                    let mut trust = content.trust().clone();
                    trust.forget(&file_host, file_port);
                    content.set_trust(trust);
                    Ok(SecurityChange::Save {
                        report: SecurityReport::TrustForgotten {
                            server: report_server,
                        },
                        records: Vec::new(),
                    })
                }),
                cx,
            );
        } else {
            self.security.dialog = None;
            self.security_set_notice(
                Tone::Positive,
                copy::fill(
                    self.security_strings().trust_deleted_note,
                    &[("server", &server)],
                ),
                cx,
            );
            cx.notify();
        }
    }

    /// Mirrors one identity a connection just accepted into the vault, so the
    /// acceptance travels with it and every other device stops asking (ADR-6:
    /// from version 2 the vault is the authority; the plain file is this
    /// device's legacy, kept in step but never the second word).
    ///
    /// The mirror never blocks the connection that asked for it: if a vault
    /// job is already running the acceptance stays in this device's file and
    /// the next one mirrors both, and a vault this machine does not have yet
    /// is created by the first record that needs one, not by a trust entry.
    pub(super) fn security_mirror_trust(&mut self, entry: &TrustEntry, cx: &mut Context<Self>) {
        if self.security.busy() || !VaultStore::at(self.store.root()).exists() {
            return;
        }
        let host = entry.host().to_owned();
        let port = entry.port();
        let key = HostKey::new(
            entry.algorithm().to_owned(),
            entry.fingerprint().to_owned(),
            entry.public_key().to_owned(),
        );
        let at = entry.accepted_unix();
        self.security_run(
            Box::new(move |content, _, _| {
                let key = key.map_err(|error| error.to_string())?;
                let mut trust = content.trust().clone();
                trust
                    .accept(&host, port, &key, at)
                    .map_err(|error| error.to_string())?;
                content.set_trust(trust);
                Ok(SecurityChange::Save {
                    report: SecurityReport::TrustMirrored,
                    records: Vec::new(),
                })
            }),
            cx,
        );
    }

    /// The refusal slot of the open dialog, when it has one.
    fn security_refusal_slot(&mut self) -> Option<&mut Option<String>> {
        match self.security.dialog.as_mut() {
            Some(SecurityDialog::ImportKey { refusal, .. })
            | Some(SecurityDialog::GenerateKey { refusal, .. })
            | Some(SecurityDialog::EditKey { refusal, .. })
            | Some(SecurityDialog::ExportKey { refusal, .. })
            | Some(SecurityDialog::RotateKey { refusal, .. })
            | Some(SecurityDialog::NewCredential { refusal, .. })
            | Some(SecurityDialog::EditCredential { refusal, .. }) => Some(refusal),
            _ => None,
        }
    }

    /// Places keyboard focus in the open dialog's first empty required field after a refusal.
    fn security_focus_missing_field(&mut self, cx: &App) {
        let target = match self.security.dialog.as_ref() {
            Some(SecurityDialog::ImportKey { pem, name, .. }) => {
                if pem.read(cx).text().trim().is_empty() {
                    Some(pem.read(cx).focus_handle().clone())
                } else {
                    Some(name.read(cx).focus_handle().clone())
                }
            }
            Some(
                SecurityDialog::GenerateKey { name, .. }
                | SecurityDialog::EditKey { name, .. }
                | SecurityDialog::RotateKey { name, .. },
            ) => Some(name.read(cx).focus_handle().clone()),
            Some(SecurityDialog::NewCredential { name, secret, .. }) => {
                if name.read(cx).text().trim().is_empty() {
                    Some(name.read(cx).focus_handle().clone())
                } else {
                    Some(secret.read(cx).focus_handle().clone())
                }
            }
            Some(SecurityDialog::EditCredential { name, .. }) => {
                Some(name.read(cx).focus_handle().clone())
            }
            Some(SecurityDialog::ExportKey { passphrase, .. }) => {
                Some(passphrase.read(cx).focus_handle().clone())
            }
            _ => None,
        };
        if target.is_some() {
            self.field_focus_pending = target;
        }
    }

    /// Opens the system's save dialog for a private key export, and exports to
    /// the file it returns. A refused or cancelled choice writes nothing.
    fn security_choose_export(&mut self, cx: &mut Context<Self>) {
        let strings = self.security_strings();
        let (key, plain, plain_confirmed, pass, second, choosing) =
            match self.security.dialog.as_ref() {
                Some(SecurityDialog::ExportKey {
                    key,
                    plain,
                    plain_confirmed,
                    passphrase,
                    confirm,
                    choosing,
                    ..
                }) => (
                    *key,
                    *plain,
                    *plain_confirmed,
                    passphrase.read(cx).text().to_owned(),
                    confirm.read(cx).text().to_owned(),
                    *choosing,
                ),
                _ => return,
            };
        if choosing || self.security.busy() {
            return;
        }
        // A private key leaving the vault is one of the operations that always
        // verify anew, whatever the policy: the gate runs before the file
        // chooser opens, so a cancelled verification leaves no file behind
        // (R3). The grant is one passage - the resumed chooser does not ask
        // for the pass it just got.
        if !self.security.operation_granted
            && self.security_begin_always(vec![SecurityPending::ExportKey], cx)
        {
            return;
        }
        self.security.operation_granted = false;
        // The two refusals an export can make before a file is ever chosen: no
        // passphrase typed twice, or a plain export without its second, explicit
        // acceptance of the risk.
        let refusal = if plain {
            (!plain_confirmed).then(|| strings.export_plain_risk.to_owned())
        } else if pass.trim().is_empty() {
            Some(strings.export_passphrase_required.to_owned())
        } else if pass != second {
            Some(strings.export_mismatch.to_owned())
        } else {
            None
        };
        if let Some(text) = refusal {
            if let Some(slot) = self.security_refusal_slot() {
                *slot = Some(text);
            }
            cx.notify();
            return;
        }
        let secret = (!plain).then(|| SecretString::new(pass));
        if let Some(SecurityDialog::ExportKey { choosing, .. }) = self.security.dialog.as_mut() {
            *choosing = true;
        }
        let directory =
            shellune_storage::home_directory().unwrap_or_else(|| std::path::PathBuf::from("."));
        let name = format!("id_shellune_{}", remote_now());
        let answer = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let chosen = match answer.await {
                Ok(Ok(Some(path))) => Some(path),
                _ => None,
            };
            let _ = this.update(cx, |view, cx| {
                if let Some(SecurityDialog::ExportKey { choosing, .. }) =
                    view.security.dialog.as_mut()
                {
                    *choosing = false;
                }
                match chosen {
                    Some(path) => view.security_export_to(key, path, secret, cx),
                    None => cx.notify(),
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Writes one private key export to `path`, on the background executor.
    ///
    /// The export passphrase is used for this export only and never saved; the
    /// file the key lands in is the one the system's dialog returned.
    pub(crate) fn security_export_to(
        &mut self,
        key: KeyId,
        path: PathBuf,
        passphrase: Option<SecretString>,
        cx: &mut Context<Self>,
    ) {
        if self.security.busy() {
            return;
        }
        self.security_run(
            Box::new(move |content, _, _| {
                let body = content
                    .key(key)
                    .and_then(|record| record.private_key().cloned())
                    .ok_or_else(|| "the vault holds no such key".to_owned())?;
                let parsed =
                    keys_api::import_private_key(&body, None).map_err(|e| e.to_string())?;
                let exported = keys_api::export_private_key(&parsed, passphrase.as_ref())
                    .map_err(|error| error.to_string())?;
                write_private_export(&path, exported.expose().as_bytes())?;
                Ok(SecurityChange::ReadOnly(SecurityReport::Exported { path }))
            }),
            cx,
        );
    }

    /// Records the page's notice for the operation that just ended.
    pub(super) fn security_set_notice(&mut self, tone: Tone, text: String, cx: &mut Context<Self>) {
        // An empty sentence is no notice: the quiet reports (a trust mirror
        // behind a connection) draw nothing rather than an empty banner.
        self.security.notice = (!text.is_empty()).then_some((tone, text));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The background job
    // -----------------------------------------------------------------------

    /// Runs one vault edit on the background executor: the page turns busy,
    /// the window keeps drawing, and the outcome arrives as a notice.
    fn security_run(&mut self, edit: SecurityEdit, cx: &mut Context<Self>) {
        self.security.notice = None;
        self.security_run_quietly(edit, cx);
    }

    /// [`Self::security_run`] for a job nobody asked for on the page - the
    /// host passwords a form or a connection hands over: the notice already
    /// drawn stays where it is.
    fn security_run_quietly(&mut self, edit: SecurityEdit, cx: &mut Context<Self>) {
        let store = VaultStore::at(self.store.root());
        let secrets = self.secrets.clone();
        let hosts = self.book.hosts().to_vec();
        let connections = self.book.connections().to_vec();
        self.security.busy = true;
        // The book as the job starts from it, so the finish can tell an edit
        // the user made meanwhile from the change the job itself made.
        self.security.job_book = Some(self.book.records());
        let work = cx
            .background_executor()
            .spawn(async move { security_apply(store, secrets, hosts, connections, edit) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| view.security_finished(result, cx));
        })
        .detach();
        cx.notify();
    }

    /// Takes in what one background job did. Records are written only
    /// after the vault was saved; a host file that cannot be written rolls the
    /// vault back to what it was, so neither file describes a state the other
    /// contradicts.
    fn security_finished(&mut self, result: SecurityResult, cx: &mut Context<Self>) {
        // A test can hold the finish of one job, the way the sync panel does, so
        // the busy state the window draws while work is in flight is a thing a
        // test can see rather than a claim about the code.
        #[cfg(test)]
        if self.security.hold_finish {
            self.security.held = Some(result);
            cx.notify();
            return;
        }
        self.security.busy = false;
        if let Some(job) = self.security.host_job.take() {
            self.host_secrets_finished(job, result, cx);
            self.security_job_idle(cx);
            return;
        }
        let strings = self.security_strings();
        match result {
            SecurityResult::Applied {
                content,
                previous,
                records,
                changed,
                report,
            } => {
                // An old entry whose password this save now holds is deleted,
                // whichever job carried it (R3).
                self.host_legacy_cleanup(&content, cx);
                // A locked vault stays locked: a write that ran behind the
                // lock's back (a trust deletion, which needs no verification
                // by ADR-6) does not silently hand the secrets back to memory.
                if self.security.lock.locked {
                    self.security.content = None;
                } else {
                    self.security.content = Some(content);
                }
                self.security.dialog = None;
                let mut drifted: Vec<String> = Vec::new();
                if !changed.is_empty() {
                    // The job changed some references, and the vault was
                    // saved against the book as the job saw it. The book may
                    // have moved since: instead of overwriting whole records
                    // with the job's copies - which would quietly revert every
                    // edit made while the Argon2 derivation ran - the job's
                    // sign-in change is replayed on each record as it stands
                    // now. Every other field the user edited is kept, and the
                    // vault stays the authority on what the references say,
                    // which is what keeps the saved mirror and the book
                    // describing one state (ADR-1).
                    let job_book = self
                        .security
                        .job_book
                        .take()
                        .unwrap_or_else(|| self.book.records());
                    let mut next = self.book.clone();
                    let mut failed = None;
                    for id in &changed {
                        let Some(post) = records.iter().find(|record| record.id() == *id) else {
                            continue;
                        };
                        let Some(pre) = job_book.iter().find(|record| record.id() == *id) else {
                            continue;
                        };
                        let Some(current) = self.book.connection(*id) else {
                            // The record was deleted while the job ran: there is
                            // nothing to replay on, and its reference row has
                            // to leave the vault's mirror as well, which the
                            // reconcile pass below does.
                            continue;
                        };
                        let mut merged = current.clone();
                        // The reference fields are the job's to say; everything
                        // else is the record's own, kept as it stands now.
                        match (&mut merged, post) {
                            (ConnectionRecord::Ssh(host), ConnectionRecord::Ssh(post)) => {
                                host.set_auth(post.auth());
                                host.set_managed_key_id(post.managed_key_id());
                                host.set_credential_id(post.credential_id());
                            }
                            (ConnectionRecord::Rdp(config), ConnectionRecord::Rdp(post)) => {
                                config.set_credential_id(post.credential_id());
                            }
                            (ConnectionRecord::Vnc(config), ConnectionRecord::Vnc(post)) => {
                                config.set_credential_id(post.credential_id());
                            }
                            // A record cannot change protocol under the job;
                            // the mismatch is not one to guess about.
                            _ => continue,
                        }
                        if current != *pre {
                            // An edit landed here while the job ran; the notice
                            // names it so the overlap is read, not silent.
                            drifted.push(current.name().to_owned());
                        }
                        if merged != current {
                            let outcome = match merged {
                                ConnectionRecord::Ssh(host) => next.update(host, *id),
                                other => next.update_connection(other, *id),
                            };
                            if let Err(error) = outcome {
                                failed = Some(error.to_string());
                                break;
                            }
                        }
                    }
                    match failed {
                        None => match self.write_hosts_file(&next) {
                            Ok(()) => {
                                self.book = next;
                                self.key_presence.forget();
                                self.note_data_changed(cx);
                                let items = self.book.hosts().to_vec();
                                self.workspace.replace_hosts(SectionStatus::Ready, items);
                            }
                            Err(error) => {
                                self.security_rollback(*previous, cx);
                                self.security_set_notice(
                                    Tone::Danger,
                                    format!("{}: {error}", strings.vault_refused),
                                    cx,
                                );
                                return;
                            }
                        },
                        Some(reason) => {
                            self.security_rollback(*previous, cx);
                            self.security_set_notice(Tone::Danger, reason, cx);
                            return;
                        }
                    }
                    // The saved mirror still describes the book the job saw. A
                    // record that vanished under the job is the one case the
                    // replay above cannot cover, so the mirror is rewritten
                    // against the book as it now stands - the vault's own save
                    // checks then see one state again.
                    if HostReference::snapshot_records(&self.book.records())
                        != HostReference::snapshot_records(&records)
                    {
                        self.security_reconcile(cx);
                    }
                }
                self.security.job_book = None;
                if matches!(
                    report,
                    SecurityReport::KeyDeleted | SecurityReport::CredentialDeleted
                ) {
                    self.security.detail = None;
                    self.security.revealed = false;
                }
                match (self.security.pending_bind.take(), &report) {
                    (
                        Some(PendingVaultBind::SshManagedKey),
                        SecurityReport::KeySaved(id)
                        | SecurityReport::ImportedWithWarning { id, .. },
                    ) => {
                        if let Some(draft) = self.hosts.draft_mut() {
                            draft.set_auth(shellune_core::AuthMethodKind::ManagedKey);
                            draft.set_managed_key(Some(*id));
                        }
                        if self.host_fields.is_some() {
                            self.focus_host_dialog = true;
                            self.focus_host_dialog_keep_return = true;
                        } else {
                            self.restore_focus_pending = true;
                        }
                    }
                    (
                        Some(PendingVaultBind::SshSharedCredential),
                        SecurityReport::CredentialSaved(id),
                    ) => {
                        if let Some(draft) = self.hosts.draft_mut() {
                            draft.set_auth(shellune_core::AuthMethodKind::Password);
                            draft.set_credential(Some(*id));
                        }
                        if self.host_fields.is_some() {
                            self.focus_host_dialog = true;
                            self.focus_host_dialog_keep_return = true;
                        } else {
                            self.restore_focus_pending = true;
                        }
                    }
                    (
                        Some(PendingVaultBind::DesktopSharedCredential),
                        SecurityReport::CredentialSaved(id),
                    ) => {
                        if let Some(draft) = self.hosts.protocol_draft_mut() {
                            draft.set_credential(Some(*id));
                        }
                        if self.protocol_fields.is_some() {
                            self.focus_host_dialog = true;
                            self.focus_host_dialog_keep_return = true;
                        } else {
                            self.restore_focus_pending = true;
                        }
                    }
                    (Some(_), _) | (None, _) => {
                        if self.host_fields.is_none()
                            && self.protocol_fields.is_none()
                            && self.backup.is_none()
                        {
                            self.modal_focus_captured = false;
                        }
                        self.restore_focus_pending = true;
                    }
                }
                let (tone, text) = if drifted.is_empty() {
                    security_report_text(&report, strings)
                } else {
                    (
                        Tone::Warning,
                        copy::fill(strings.drift_kept, &[("names", &drifted.join(", "))]),
                    )
                };
                if let SecurityReport::Rotated { public_key, .. } = report {
                    self.security.rotation_public_key = Some(public_key);
                }
                self.security_set_notice(tone, text, cx);
            }
            SecurityResult::Refused(reason) => {
                // A refusal keeps the dialog where it was, so the correction is
                // typed into the fields that produced it.
                match self.security.dialog.as_mut() {
                    Some(
                        SecurityDialog::ImportKey { refusal, .. }
                        | SecurityDialog::GenerateKey { refusal, .. }
                        | SecurityDialog::EditKey { refusal, .. }
                        | SecurityDialog::ExportKey { refusal, .. }
                        | SecurityDialog::RotateKey { refusal, .. }
                        | SecurityDialog::NewCredential { refusal, .. }
                        | SecurityDialog::EditCredential { refusal, .. },
                    ) => {
                        *refusal = Some(reason);
                    }
                    _ => {
                        self.security_set_notice(Tone::Warning, reason, cx);
                    }
                }
                cx.notify();
            }
            SecurityResult::Failed(reason) => {
                self.security_set_notice(
                    Tone::Danger,
                    format!("{}: {reason}", strings.vault_failed),
                    cx,
                );
            }
        }
        // Host passwords that waited for this job go next, as one more job.
        self.security_job_idle(cx);
    }

    /// Rewrites the vault's reference mirror against the book as it now
    /// stands, quietly.
    ///
    /// The bookkeeping half of a finish whose book moved under the job: the
    /// vault was saved against the book the job saw, and a host that vanished
    /// or changed references meanwhile is not one the replay above can carry.
    /// The rewrite says nothing of its own - the notice of the change the user
    /// made is already on the page - but it holds the one-job rule until it
    /// ends, so no second save can race it.
    fn security_reconcile(&mut self, cx: &mut Context<Self>) {
        let Some(mut content) = self.security.content.clone() else {
            return;
        };
        let store = VaultStore::at(self.store.root());
        let secrets = self.secrets.clone();
        let records = records_for_vault_mirror(&content, &content, &self.book.records());
        content.set_references(&HostReference::snapshot_records(&records));
        content.prune_host_secrets(&records);
        self.security.busy = true;
        let work = cx.background_executor().spawn(async move {
            store
                .save_records(
                    &content,
                    &records,
                    &vault_unseal_sources(&store, secrets.as_ref()),
                    KdfParams::default(),
                    secrets.as_ref(),
                )
                .map(|()| content)
        });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            let _ = this.update(cx, |view, cx| {
                view.security.busy = false;
                match outcome {
                    Ok(content) => {
                        if !view.security.lock.locked {
                            view.security.content = Some(content);
                        }
                    }
                    Err(reason) => view.security_set_notice(
                        Tone::Danger,
                        format!("{}: {reason}", view.security_strings().vault_failed),
                        cx,
                    ),
                }
                view.security_job_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts the vault back to what it was, against the host book that is still
    /// on disk - the undo of a change whose host file could not be written.
    fn security_rollback(&mut self, previous: VaultContent, cx: &mut Context<Self>) {
        let store = VaultStore::at(self.store.root());
        let secrets = self.secrets.clone();
        let records = records_for_vault_mirror(&previous, &previous, &self.book.records());
        self.security.busy = true;
        let work = cx.background_executor().spawn(async move {
            store
                .save_records(
                    &previous,
                    &records,
                    &vault_unseal_sources(&store, secrets.as_ref()),
                    KdfParams::default(),
                    secrets.as_ref(),
                )
                .map(|()| previous)
        });
        cx.spawn(async move |this, cx| {
            let restored = work.await;
            let _ = this.update(cx, |view, cx| {
                view.security.busy = false;
                match restored {
                    Ok(content) => {
                        if !view.security.lock.locked {
                            view.security.content = Some(content);
                        }
                    }
                    Err(reason) => view.security_set_notice(
                        Tone::Danger,
                        format!("{}: {reason}", view.security_strings().vault_failed),
                        cx,
                    ),
                }
                view.security_job_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens this machine's vault at start-up, so a connection that needs it
    /// does not wait for the settings page to be visited first. A machine with
    /// no vault yet stays without one until the first record creates it.
    ///
    /// A device whose lock is engaged does not keep what it opened: the vault
    /// starts locked and one verification opens it (R3, ADR-4), so the open
    /// here only checks the file is readable and marks the lock.
    pub(super) fn vault_warm(&mut self, cx: &mut Context<Self>) {
        if self.security.content.is_some() || self.security.warming {
            return;
        }
        if self.security.lock.engaged() {
            self.security.lock.locked = true;
            cx.notify();
            return;
        }
        self.vault_open_quietly(cx);
    }

    /// Opens the vault file in the background and keeps what it held, without
    /// touching the lock: the start-up open of a device without the lock, the
    /// second half of an unlock, and the re-read after a sync applied a vault.
    pub(super) fn vault_open_quietly(&mut self, cx: &mut Context<Self>) {
        if self.security.lock.locked {
            return;
        }
        self.security.vault_epoch = self.security.vault_epoch.wrapping_add(1);
        let epoch = self.security.vault_epoch;
        self.security.warming = true;
        let store = VaultStore::at(self.store.root());
        let secrets = self.secrets.clone();
        let work = cx.background_executor().spawn(async move {
            match store.open(&UnsealSource::LocalMachine, secrets.as_ref()) {
                Ok(content) => Ok(Some(content)),
                Err(VaultStoreError::Missing { .. }) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let opened = work.await;
            let _ = this.update(cx, |view, cx| {
                view.vault_open_quietly_finished(epoch, opened, cx);
            });
        })
        .detach();
    }

    fn vault_open_quietly_finished(
        &mut self,
        epoch: u64,
        opened: Result<Option<VaultContent>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.security.vault_epoch != epoch || self.security.lock.locked {
            return;
        }
        #[cfg(test)]
        if self.security.hold_open {
            self.security.held_open = Some((epoch, opened));
            cx.notify();
            return;
        }
        self.security.warming = false;
        if opened.is_ok() {
            self.security.open_problem = None;
        }
        match opened {
            Ok(Some(content)) => {
                let first_live_key = content
                    .keys()
                    .filter(|record| record.meta().is_live())
                    .min_by(|a, b| a.meta().name().cmp(b.meta().name()))
                    .map(|record| record.meta().id());
                // Re-reading what a sync or an import wrote touches no old
                // `host-<uuid>` entry (R4): those go with the host's own
                // deletion, untick, form save or use-time migration.
                self.security.content = Some(content);
                if let Some(first) = first_live_key
                    && let Some(draft) = self.hosts.draft_mut()
                    && draft.auth() == shellune_core::AuthMethodKind::ManagedKey
                    && draft.managed_key_id().is_none()
                {
                    draft.set_managed_key(Some(first));
                }
                // An unlock's follow-up steps waited for exactly this
                // file; they continue now that the vault is open, unless an
                // active verification prompt has not yet been answered.
                if !self.security.lock.verifying {
                    let waiting = std::mem::take(&mut self.security.lock.pending);
                    if !waiting.is_empty() {
                        self.security_resume(waiting, cx);
                    }
                }
            }
            Ok(None) => {
                // No vault exists: the steps that needed one simply
                // find nothing, once no verification prompt is still open.
                if !self.security.lock.verifying {
                    let waiting = std::mem::take(&mut self.security.lock.pending);
                    if !waiting.is_empty() {
                        self.security_resume(waiting, cx);
                    }
                }
            }
            Err(reason) => {
                // A vault that cannot be opened answers no request;
                // what waited for it dies with the reason shown.
                if self.security.lock.verifying {
                    self.security.verify_epoch = self.security.verify_epoch.wrapping_add(1);
                    self.security.lock.verifying = false;
                    #[cfg(test)]
                    {
                        self.security.held_verify = None;
                    }
                }
                self.security_abort_pending(&reason);
                self.security.open_problem = Some(reason);
            }
        }
        // Host passwords queued while the vault was being opened are written
        // now that it is.
        self.host_secrets_flush(cx);
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The lock of this device (R3, R4, ADR-4)
    // -----------------------------------------------------------------------

    /// Whether the vault of this device is locked: no secret is in memory, new
    /// secret reads and managed-key sign-ins are refused until it is unlocked.
    pub(crate) fn vault_locked(&self) -> bool {
        self.security.lock.locked
    }

    /// Re-reads the vault file into memory, dropping the copy the window held:
    /// what a sync (or any other writer outside this window) saved is what the
    /// connections and the pages see from now on. A locked device stays
    /// locked; only the file changed, not the memory.
    pub(super) fn vault_reread(&mut self, cx: &mut Context<Self>) {
        if self.security.lock.locked {
            return;
        }
        self.security.content = None;
        self.vault_open_quietly(cx);
    }

    /// Locks the vault now: the opened content leaves memory and the next
    /// secret read waits for one verification. Established sessions keep what
    /// they already have - a lock governs what may be read from now on, and
    /// never tears a live connection down (R3).
    pub(super) fn security_lock_now(&mut self, cx: &mut Context<Self>) {
        let cancel_reason = self.security_strings().lock_cancelled.to_owned();
        self.security.vault_epoch = self.security.vault_epoch.wrapping_add(1);
        self.security.verify_epoch = self.security.verify_epoch.wrapping_add(1);
        self.security.warming = false;
        self.security.lock.verifying = false;
        #[cfg(test)]
        {
            self.security.held_verify = None;
            self.security.held_open = None;
        }
        if !self.security.lock.pending.is_empty() {
            self.security_abort_pending(&cancel_reason);
        }
        // What the credential store handed over this session is dropped with
        // the content: the next unlock reads it again (ADR-3, R6).
        self.secrets.clear_cache();
        if !self.security.lock.locked {
            self.security.lock.locked = true;
            self.security.lock.idle_deadline = None;
            self.security.lock.idle_generation += 1;
            self.security.operation_granted = false;
            self.security.content = None;
            self.security.revealed = false;
            self.security_set_notice(
                Tone::Neutral,
                self.security_strings().lock_state_locked.to_owned(),
                cx,
            );
        }
        cx.notify();
    }

    /// Fails every queued connection step waiting on a verification or vault open.
    fn security_abort_pending(&mut self, reason: &str) {
        let aborted = std::mem::take(&mut self.security.lock.pending);
        for step in aborted {
            match step {
                SecurityPending::ResumeChain { session, host, .. } => {
                    self.abort_chain_after_verify(session, host, reason);
                }
                SecurityPending::Desktop(session) => {
                    self.security_fail_desktop(session, reason.to_owned());
                }
                SecurityPending::HostSecrets => {
                    self.host_form_save_failed(reason);
                }
                _ => {}
            }
        }
    }

    /// Marks `session` failed due to a vault verification cancellation/failure and
    /// stores the human-readable reason so `failure_notice` renders it in-place.
    pub(crate) fn security_fail_desktop(
        &mut self,
        session: shellune_core::SessionId,
        reason: String,
    ) {
        self.forget_session_desktop_secret(session);
        if self.workspace.session(session).is_none() {
            return;
        }
        self.security
            .session_verify_failures
            .insert(session, reason.clone());
        self.workspace.session_failed(
            session,
            shellune_core::SessionFailure::ConnectionFailed { reason },
        );
    }

    /// Clears any recorded verification failure reason for `session` when starting anew.
    pub(crate) fn security_clear_session_failure(&mut self, session: shellune_core::SessionId) {
        self.security.session_verify_failures.remove(&session);
    }

    /// Returns the in-place vault verification failure reason recorded for `session`, if any.
    pub(crate) fn security_session_failure_reason(
        &self,
        session: shellune_core::SessionId,
    ) -> Option<&str> {
        self.security
            .session_verify_failures
            .get(&session)
            .map(String::as_str)
    }

    /// Notes a sensitive use and schedules the idle lock from now (R3). A
    /// system sleep counts: the deadline is wall clock, so a machine that
    /// slept past it locks the moment it is back.
    pub(super) fn security_touch(&mut self, cx: &mut Context<Self>) {
        let lock = &mut self.security.lock;
        if !lock.engaged() || lock.settings.policy.is_disabled() {
            lock.idle_deadline = None;
            return;
        }
        let wait = lock.idle_lock();
        lock.idle_deadline = Some(SystemTime::now() + wait);
        lock.idle_generation += 1;
        let generation = lock.idle_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |view, cx| {
                let lock = &mut view.security.lock;
                if lock.idle_generation != generation {
                    return;
                }
                lock.idle_deadline = None;
                if !lock.locked {
                    // A timer that still owns the generation is the idle time
                    // passing without a sensitive use: a system sleep holds
                    // the executor's timers and releases them on wake, so the
                    // lock lands then too (R3).
                    view.security_lock_now(cx);
                }
            });
        })
        .detach();
    }

    /// Shows the password of one record, behind the gate the lock asks for
    /// (R3): the "every sensitive operation" policy verifies afresh each time,
    /// and a locked vault unlocks first.
    fn security_reveal(&mut self, credential: CredentialId, cx: &mut Context<Self>) {
        if self.workspace.destination() == super::Destination::Settings {
            self.settings_note_keys_item();
        }
        if self.security_begin(vec![SecurityPending::Reveal(credential)], cx) {
            return;
        }
        self.security.revealed = true;
        self.security.detail = Some(SecurityRecord::Credential(credential));
        self.security_touch(cx);
        cx.notify();
    }

    /// Copies the password of one record without drawing it, behind the same
    /// gate as showing it (R3).
    fn security_copy_password(&mut self, credential: CredentialId, cx: &mut Context<Self>) {
        if self.security_begin(vec![SecurityPending::Copy(credential)], cx) {
            return;
        }
        if let Some(text) = self
            .security
            .content
            .as_ref()
            .and_then(|vault| vault.credential(credential))
            .and_then(|record| record.secret())
            .map(|secret| secret.expose().to_owned())
        {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            let copied_title = self.security_strings().password_copied;
            self.toast(Tone::Positive, copied_title, None, cx);
        }
        self.security_touch(cx);
    }

    /// Runs `steps` behind the gate the lock asks for, or right away when it
    /// asks for nothing. Returns whether the steps wait for a verification
    /// (the caller does nothing more in that case: the queue continues on its
    /// own, or dies with the prompt).
    ///
    /// - the lock off: nothing is asked, the steps run at once - nobody is
    ///   forced to set a password for daily local use (R3);
    /// - the vault locked: one verification opens it and the steps follow;
    /// - every sensitive operation: one fresh verification per request, and a
    ///   success is never carried to the next one (R3).
    pub(super) fn security_begin(
        &mut self,
        steps: Vec<SecurityPending>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.security_begin_when(false, steps, cx)
    }

    /// [`Self::security_begin`] for the operations that verify anew in both
    /// modes - a private key export, resetting the biometric protection (R3).
    pub(super) fn security_begin_always(
        &mut self,
        steps: Vec<SecurityPending>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.security_begin_when(true, steps, cx)
    }

    /// The one gate both entry points share.
    fn security_begin_when(
        &mut self,
        always: bool,
        steps: Vec<SecurityPending>,
        cx: &mut Context<Self>,
    ) -> bool {
        let lock = &self.security.lock;
        let requires_verify =
            lock.locked || (lock.engaged() && (lock.verifies_every_operation() || always));
        if requires_verify {
            if self.security_gate_busy() {
                // One prompt (or already-verified warming queue) at a time; a
                // second request while the gate is occupied is refused rather
                // than overwriting or queueing behind a prompt the user may cancel.
                let strings = self.security_strings();
                self.security_set_notice(Tone::Warning, strings.lock_busy.to_owned(), cx);
                return true;
            }
            let mut queue = Vec::with_capacity(steps.len() + 1);
            if lock.locked {
                queue.push(SecurityPending::Unlock);
            }
            queue.extend(steps);
            self.security.lock.pending = queue;
            self.security_verify(cx);
            return true;
        }
        if !lock.locked && self.security.content.is_none() && self.security.warming {
            self.security.lock.pending.extend(steps);
            cx.notify();
            return true;
        }
        false
    }

    /// Asks the system to verify the user once, on the background executor:
    /// some platform prompts wait for the person to answer.
    fn security_verify(&mut self, cx: &mut Context<Self>) {
        self.security.verify_epoch = self.security.verify_epoch.wrapping_add(1);
        let epoch = self.security.verify_epoch;
        self.security.lock.verifying = true;
        let authenticator = self.security.lock.authenticator.clone();
        let prompt = VerificationPrompt::new(self.security_strings().lock_unlock.to_owned());
        let work = cx
            .background_executor()
            .spawn(async move { authenticator.verify(&prompt) });
        cx.spawn(async move |this, cx| {
            let answer = work.await;
            let _ = this.update(cx, |view, cx| {
                view.security_verify_finished(epoch, answer, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Takes in the system's answer to the one verification in flight. A pass
    /// runs what waited; anything else keeps the vault exactly as it was - a
    /// cancellation, a mismatch or a busy system is never a pass (R3, R4).
    fn security_verify_finished(
        &mut self,
        epoch: u64,
        answer: Result<(), BiometricError>,
        cx: &mut Context<Self>,
    ) {
        if self.security.verify_epoch != epoch || !self.security.lock.verifying {
            return;
        }
        #[cfg(test)]
        if self.security.hold_verify {
            self.security.held_verify = Some((epoch, answer));
            cx.notify();
            return;
        }
        self.security.lock.verifying = false;
        let strings = self.security_strings();
        match answer {
            Ok(()) => {
                let queue = std::mem::take(&mut self.security.lock.pending);
                self.security_resume(queue, cx);
            }
            Err(error) => {
                let reason = if error.is_enrollment_changed() {
                    // The biometric set changed, so this device's protection
                    // no longer holds: biometric unlocking is turned off here
                    // until the user re-enables it with a fresh verification
                    // (R4), and the vault stays as it was.
                    self.security_set_biometric(false, cx);
                    strings.lock_enrollment_changed.to_owned()
                } else if error.is_cancelled() {
                    strings.lock_cancelled.to_owned()
                } else {
                    match &error {
                        BiometricError::Busy { .. } => strings.lock_busy.to_owned(),
                        _ => format!("{} {}", strings.lock_failed, error),
                    }
                };
                self.security_set_notice(Tone::Warning, reason.clone(), cx);
                self.security_abort_pending(&reason);
            }
        }
        cx.notify();
    }

    /// Writes the biometric switch of this device to its own file and this
    /// window's state. Nothing here asks the system anything: the callers
    /// either just verified, or are turning a dead setting off.
    fn security_set_biometric(&mut self, on: bool, cx: &mut Context<Self>) {
        self.security.lock.settings.biometric_unlock = Some(on);
        let store = SecurityStore::at(self.store.root());
        if let Err(error) = store.save(&self.security.lock.settings) {
            self.security_set_notice(
                Tone::Danger,
                format!("{}: {error}", self.security_strings().vault_failed),
                cx,
            );
            return;
        }
        self.security.lock.problem = None;
        if !on && !self.security.lock.locked {
            // Turning the protection off while the vault is open leaves it
            // open the plain way - the machine's own unseal secret is the
            // whole protection now, and every function stays available (R3).
            // A locked vault is not opened by this: the one path here that
            // reaches a locked vault is an enrollment change, and that vault
            // waits for a fresh verification (re-enabling, behind the switch)
            // rather than falling back on silence (R4).
            if self.security.content.is_none() && !self.security.warming {
                self.vault_open_quietly(cx);
            }
        }
        cx.notify();
    }

    /// Saves the lock choices of this device after a page edit. The policy
    /// itself never prompts: it is a choice about this machine, not a secret.
    pub(super) fn security_save_lock_settings(&mut self, cx: &mut Context<Self>) {
        let store = SecurityStore::at(self.store.root());
        if let Err(error) = store.save(&self.security.lock.settings) {
            self.security_set_notice(
                Tone::Danger,
                format!("{}: {error}", self.security_strings().vault_failed),
                cx,
            );
            return;
        }
        self.security.lock.problem = None;
        cx.notify();
    }

    /// Turns the biometric switch, behind one fresh verification in both
    /// directions: re-setting the biometric protection is one of the
    /// operations that always verify anew (R3).
    pub(super) fn security_toggle_biometric(&mut self, on: bool, cx: &mut Context<Self>) {
        let current = self.security.lock.settings.biometric_unlock == Some(true);
        if on == current || self.security_gate_busy() {
            return;
        }
        if !on {
            // The session-only shape has no prompt to ask: turning it off is
            // the unlocked session's own decision, and demanding a
            // verification nothing can answer were the same lock-out in the
            // other direction (R4, P1-2).
            if !self.security.lock.session_only()
                && self.security_begin_always(vec![SecurityPending::DisableBiometric], cx)
            {
                return;
            }
            self.security_set_biometric(false, cx);
            return;
        }
        // Turning it on asks the system first, the way the storage layer's
        // fallback model says (R4): a system that can verify gets one fresh
        // verification, and a system with nothing to call gets the unlocked
        // session behind an explicit risk confirmation - never a silent
        // enable, and never a lock there is no way out of (P1-2).
        let capability = self.security.lock.authenticator.capability();
        match LocalOnlyFallback::for_capability(capability) {
            LocalOnlyFallback::SystemUserVerification => {
                self.security.lock.pending = vec![SecurityPending::EnableBiometric];
                self.security_verify(cx);
            }
            LocalOnlyFallback::UnlockedSessionWithConfirmation => {
                if self.security.dialog.is_some() {
                    return;
                }
                self.security.dialog = Some(SecurityDialog::BiometricRisk);
                cx.notify();
            }
        }
    }

    /// Enables the biometric switch in the shape a machine without any system
    /// verification can have: the unlocked session plus the user's explicit
    /// acceptance, recorded in this device's own file. The lock never
    /// auto-locks in this shape - there is nothing to unlock it with (R4,
    /// `LocalOnlyFallback::UnlockedSessionWithConfirmation`).
    pub(super) fn security_enable_session_only(&mut self, cx: &mut Context<Self>) {
        self.security.dialog = None;
        self.security.lock.settings.biometric_unlock = Some(true);
        let store = SecurityStore::at(self.store.root());
        if let Err(error) = store.save(&self.security.lock.settings) {
            self.security_set_notice(
                Tone::Danger,
                format!("{}: {error}", self.security_strings().vault_failed),
                cx,
            );
            return;
        }
        self.security.lock.problem = None;
        cx.notify();
    }
    pub(super) fn security_set_lock_policy(&mut self, every: bool, cx: &mut Context<Self>) {
        self.security.lock.settings.policy = if every {
            LockPolicy::EverySensitiveOperation
        } else {
            LockPolicy::UnlockVault
        };
        self.security_save_lock_settings(cx);
        self.security_touch(cx);
    }

    /// The lock page's idle minutes, as far as they parse.
    pub(super) fn security_set_idle_minutes(
        &mut self,
        minutes: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let strings = self.security_strings();
        let Ok(value) = minutes.trim().parse::<u16>() else {
            self.security_set_notice(Tone::Warning, strings.lock_idle_refusal.to_owned(), cx);
            return false;
        };
        if !(1..=1440).contains(&value) {
            self.security_set_notice(Tone::Warning, strings.lock_idle_refusal.to_owned(), cx);
            return false;
        }
        self.security.lock.settings.idle_lock_minutes = Some(value);
        self.security_save_lock_settings(cx);
        self.security_touch(cx);
        true
    }

    /// Runs what a granted verification waited for, in order. The queue is the
    /// only thing a pass buys: by the time it ends there is no standing
    /// permission, so the next request asks the system again (R3).
    fn security_resume(&mut self, queue: Vec<SecurityPending>, cx: &mut Context<Self>) {
        let mut queue = queue.into_iter();
        while let Some(step) = queue.next() {
            let needs_vault_content = matches!(
                step,
                SecurityPending::Reveal(_)
                    | SecurityPending::Copy(_)
                    | SecurityPending::ResumeChain { .. }
                    | SecurityPending::Desktop(_)
                    | SecurityPending::ExportKey
                    | SecurityPending::OpenAction(_)
                    | SecurityPending::HostSecrets
                    | SecurityPending::Batch
                    | SecurityPending::BackupSecretsExport
            );
            if needs_vault_content
                && !self.security.lock.locked
                && self.security.content.is_none()
                && self.security.warming
            {
                let mut deferred = Vec::with_capacity(queue.len() + 1);
                deferred.push(step);
                deferred.extend(queue);
                self.security.lock.pending = deferred;
                cx.notify();
                return;
            }
            match step {
                SecurityPending::Unlock => {
                    if self.security.lock.locked {
                        self.security.lock.locked = false;
                        self.security_set_notice(
                            Tone::Positive,
                            self.security_strings().lock_state_open.to_owned(),
                            cx,
                        );
                        // The idle clock starts with the unlock, and an
                        // automatic sync that waited for it starts now, not
                        // at the next poll (ADR-4).
                        self.security_touch(cx);
                        self.resume_deferred_sync(cx);
                    }
                    if self.security.content.is_none() {
                        // The rest of the queue needs the file open; it waits
                        // for the background read and continues there.
                        self.security.lock.pending = queue.collect();
                        if !self.security.warming {
                            self.vault_open_quietly(cx);
                        }
                        return;
                    }
                }
                SecurityPending::Reveal(credential) => {
                    if self.is_security_keys_surface_visible() {
                        self.security.revealed = true;
                        self.security.detail = Some(SecurityRecord::Credential(credential));
                        self.security_touch(cx);
                    }
                }
                SecurityPending::Copy(credential) => {
                    if let Some(text) = self
                        .security
                        .content
                        .as_ref()
                        .and_then(|vault| vault.credential(credential))
                        .and_then(|record| record.secret())
                        .map(|secret| secret.expose().to_owned())
                    {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        let copied_title = self.security_strings().password_copied;
                        self.toast(Tone::Positive, copied_title, None, cx);
                        self.security_touch(cx);
                    }
                }
                SecurityPending::ResumeChain {
                    session,
                    host,
                    take_panel,
                } => {
                    // One passage of the connection gate, already paid for:
                    // the dial continues where it was interrupted (R3).
                    self.security.operation_granted = true;
                    self.security_touch(cx);
                    self.resume_chain_after_verify(session, host, take_panel, cx);
                    self.security.operation_granted = false;
                }
                SecurityPending::Desktop(session) => {
                    self.security.operation_granted = true;
                    self.security_touch(cx);
                    self.start_desktop_session(session, cx);
                    self.security.operation_granted = false;
                }
                SecurityPending::ExportKey => {
                    // One passage of the export gate, already paid for: the
                    // chooser opens without asking again (R3).
                    self.security.operation_granted = true;
                    self.security_touch(cx);
                    self.security_choose_export(cx);
                    self.security.operation_granted = false;
                }
                SecurityPending::BackupVaultExport => {
                    self.security.operation_granted = true;
                    self.security_touch(cx);
                    self.choose_backup_export_path(cx);
                    self.security.operation_granted = false;
                }
                SecurityPending::BackupSecretsExport => {
                    self.security_touch(cx);
                    self.choose_backup_export_path(cx);
                }
                SecurityPending::EnableBiometric => {
                    self.security_set_biometric(true, cx);
                    self.security_touch(cx);
                }
                SecurityPending::DisableBiometric => {
                    self.security_set_biometric(false, cx);
                }
                SecurityPending::OpenAction(action) => {
                    self.security_start(action, cx);
                }
                SecurityPending::HostSecrets => {
                    self.host_secrets_flush(cx);
                }
                SecurityPending::Batch => {
                    self.confirm_batch(cx);
                }
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Cross-surface vault queries & entry points
    // -----------------------------------------------------------------------

    /// Total live keys and shared passwords currently loaded in the unlocked vault.
    pub(crate) fn security_record_count(&self) -> usize {
        let Some(content) = self.security.content.as_ref() else {
            return 0;
        };
        content.keys().filter(|r| r.meta().is_live()).count()
            + content.credentials().filter(|r| r.meta().is_live()).count()
    }

    /// Whether the topbar should display the vault lock/unlock quick control.
    pub(crate) fn security_topbar_lock_shown(&self) -> bool {
        self.security.lock.engaged() || self.security.lock.locked
    }

    /// Whether a system/biometric verification prompt (or a verified queue waiting on
    /// background vault warming) currently occupies the security gate.
    pub(crate) fn security_gate_busy(&self) -> bool {
        self.security.lock.verifying
            || (self.security.warming && !self.security.lock.pending.is_empty())
    }

    /// Whether a system/biometric verification prompt is currently in flight.
    pub(crate) fn security_verifying(&self) -> bool {
        self.security.lock.verifying
    }

    /// Focuses `security.list_focus` without altering the current cursor.
    pub(crate) fn security_focus_list_handle(&self, window: &mut Window) {
        window.focus(&self.security.list_focus);
    }

    /// Whether the currently displayed SSH/chain connection attempt is waiting on vault verification/open.
    pub(crate) fn security_verifying_for_chain(&self) -> bool {
        let Some((session, host)) = self.current_chain_target() else {
            return false;
        };
        (self.security.lock.verifying || self.security.warming)
            && self.security.lock.pending.iter().any(|step| {
                matches!(
                    step,
                    SecurityPending::ResumeChain {
                        session: pending_session,
                        host: pending_host,
                        ..
                    } if *pending_session == session && *pending_host == host
                )
            })
    }

    /// Binds a newly registered `SessionId` onto an in-flight `ResumeChain` when an
    /// unassigned foreground connection form gets parked onto its own session tab.
    pub(crate) fn security_bind_parked_chain(
        &mut self,
        host: HostId,
        session: shellune_core::SessionId,
    ) {
        for step in &mut self.security.lock.pending {
            if let SecurityPending::ResumeChain {
                session: slot @ None,
                host: pending_host,
                ..
            } = step
                && *pending_host == host
            {
                *slot = Some(session);
            }
        }
    }

    /// Drops any queued verification continuation and stale verification failure record
    /// owned by a session that was closed.
    pub(crate) fn security_drop_pending_session(&mut self, session: shellune_core::SessionId) {
        self.security.session_verify_failures.remove(&session);
        self.security.lock.pending.retain(|step| match step {
            SecurityPending::Desktop(id) => *id != session,
            SecurityPending::ResumeChain {
                session: Some(id), ..
            } => *id != session,
            _ => true,
        });
    }

    /// Drops any unassigned foreground `ResumeChain` step when its connection dialog is cancelled.
    pub(crate) fn security_drop_unassigned_chain(&mut self) {
        self.security
            .lock
            .pending
            .retain(|step| !matches!(step, SecurityPending::ResumeChain { session: None, .. }));
    }

    /// Whether `session` (an RDP/VNC desktop session) is waiting on vault verification/open.
    pub(crate) fn security_verifying_for_desktop(&self, session: shellune_core::SessionId) -> bool {
        (self.security.lock.verifying || self.security.warming)
            && self
                .security
                .lock
                .pending
                .iter()
                .any(|step| matches!(step, SecurityPending::Desktop(id) if *id == session))
    }

    /// Whether any desktop session is currently waiting on vault verification/open, returning its ID.
    pub(crate) fn security_verifying_desktop_session(&self) -> Option<shellune_core::SessionId> {
        if !(self.security.lock.verifying || self.security.warming) {
            return None;
        }
        self.security
            .lock
            .pending
            .iter()
            .find_map(|step| match step {
                SecurityPending::Desktop(id) => Some(*id),
                _ => None,
            })
    }

    /// Whether a vault modal dialog is currently open.
    pub(crate) fn security_dialog_open(&self) -> bool {
        self.security.dialog.is_some()
    }

    /// Whether `handle` belongs to one of the text inputs owned by the open vault dialog.
    pub(crate) fn security_dialog_owns_focus(&self, handle: &gpui::FocusHandle, cx: &App) -> bool {
        self.security
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.owns_focus(handle, cx))
    }

    /// Focus handle of the vault search input.
    pub(crate) fn security_search_focus(&self, cx: &App) -> gpui::FocusHandle {
        self.security.search.read(cx).focus_handle().clone()
    }

    /// Whether the vault list focus handle currently holds keyboard focus.
    pub(crate) fn security_list_focused(&self, window: &Window) -> bool {
        self.security.list_focus.is_focused(window)
    }

    /// Clears the vault search input on Escape when it is focused and non-empty.
    pub(crate) fn security_clear_search_if_focused(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .security
            .search
            .read(cx)
            .focus_handle()
            .is_focused(window)
            && !self.security.search.read(cx).text().is_empty()
        {
            let search = self.security.search.clone();
            search.update(cx, |field, cx| field.set_text("", cx));
            self.security.revealed = false;
            cx.notify();
            return true;
        }
        false
    }

    /// Whether `id` is known to be missing from the unlocked vault.
    pub(crate) fn security_vault_key_missing(&self, id: KeyId) -> bool {
        if self.security.lock.locked || self.security.warming {
            return false;
        }
        match self.security.content.as_ref() {
            Some(vault) => vault.key(id).is_none_or(|record| !record.meta().is_live()),
            None => !VaultStore::at(self.store.root()).exists(),
        }
    }

    /// Whether `id` is known to be missing from the unlocked vault.
    pub(crate) fn security_vault_credential_missing(&self, id: CredentialId) -> bool {
        if self.security.lock.locked || self.security.warming {
            return false;
        }
        match self.security.content.as_ref() {
            Some(vault) => vault
                .credential(id)
                .is_none_or(|record| !record.meta().is_live()),
            None => !VaultStore::at(self.store.root()).exists(),
        }
    }

    /// Opens a vault creation dialog stacked above an open connection draft,
    /// auto-binding the saved record back into that draft.
    pub(crate) fn security_open_inline(
        &mut self,
        action: SecurityAction,
        bind: PendingVaultBind,
        cx: &mut Context<Self>,
    ) {
        if self.security.busy() || self.security.dialog.is_some() {
            return;
        }
        self.security.pending_bind = Some(bind);
        self.security_start(action, cx);
    }

    /// Opens a vault creation dialog from the Command Palette, unlocking first if locked.
    pub(crate) fn security_open_palette_action(
        &mut self,
        action: SecurityAction,
        cx: &mut Context<Self>,
    ) {
        self.security.pending_bind = None;
        if self.security.lock.locked {
            self.security_begin(vec![SecurityPending::OpenAction(action)], cx);
            return;
        }
        self.security_start(action, cx);
    }

    // -----------------------------------------------------------------------
    // Dialog drawing
    // -----------------------------------------------------------------------

    /// The page's dialog, while one is open.
    pub(super) fn security_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let dialog = self.security.dialog.as_ref()?;
        let strings = self.security_strings();
        let metrics = self.metrics();
        let (title, intro, icon, body, footer) = match dialog {
            SecurityDialog::ImportKey { refusal, .. } => {
                let browse = components::icon_label_button(
                    Icon::FolderOpen,
                    strings.import_browse_file,
                    Variant::Secondary,
                    metrics,
                )
                .id("security.dialog.import.browse")
                .debug_selector(|| "security.dialog.import.browse".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.security_browse_import_file(cx);
                }));
                let body = self
                    .security_dialog_body("security.dialog.import", cx)
                    .child(div().flex().items_center().child(browse))
                    .child(self.security_dialog_field(
                        "security.dialog.pem",
                        strings.import_pem,
                        dialog_pem(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.passphrase",
                        strings.import_passphrase,
                        dialog_passphrase(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.import_name,
                        dialog_name(dialog),
                    ))
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    );
                let footer = self.security_dialog_footer(strings.import_key, Variant::Primary, cx);
                (
                    strings.import_title.to_owned(),
                    Some(strings.import_intro.to_owned()),
                    Icon::Import,
                    body,
                    footer,
                )
            }
            SecurityDialog::GenerateKey { kind, refusal, .. } => {
                let mut body = self
                    .security_dialog_body("security.dialog.generate", cx)
                    .child(self.security_kind_segments(*kind, cx))
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.generate_name,
                        dialog_name(dialog),
                    ));
                if let Some(comment) = dialog_comment(dialog) {
                    body = body.child(self.security_dialog_field(
                        "security.dialog.comment",
                        strings.generate_comment,
                        Some(comment),
                    ));
                }
                body = body.children(
                    refusal
                        .clone()
                        .map(|reason| security_refusal_banner(reason, metrics)),
                );
                let footer =
                    self.security_dialog_footer(strings.generate_key, Variant::Primary, cx);
                (
                    strings.generate_title.to_owned(),
                    None,
                    Icon::Plus,
                    body,
                    footer,
                )
            }
            SecurityDialog::EditKey { refusal, .. } => {
                let body = self
                    .security_dialog_body("security.dialog.edit-key", cx)
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.credential_name,
                        dialog_name(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.comment",
                        strings.label_comment,
                        dialog_comment(dialog),
                    ))
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    );
                let footer = self.security_dialog_footer(strings.save, Variant::Primary, cx);
                (
                    strings.edit_key_title.to_owned(),
                    Some(strings.edit_key_note.to_owned()),
                    Icon::Pencil,
                    body,
                    footer,
                )
            }
            SecurityDialog::ExportKey {
                plain,
                plain_confirmed,
                refusal,
                saved_to,
                ..
            } => {
                let mut body = self
                    .security_dialog_body("security.dialog.export", cx)
                    .child(
                        checkbox(
                            "security.dialog.export.plain",
                            strings.export_plain,
                            *plain,
                            metrics,
                        )
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_toggle_plain_export(cx);
                            },
                        )),
                    );
                if *plain {
                    body = body
                        .child(
                            div()
                                .text_size(metrics.caption_size())
                                .text_color(theme::Tone::Warning.text())
                                .debug_selector(|| "security.dialog.export.risk".to_owned())
                                .child(strings.export_plain_risk),
                        )
                        .child(
                            checkbox(
                                "security.dialog.export.plain.confirm",
                                strings.export_plain_confirm,
                                *plain_confirmed,
                                metrics,
                            )
                            .on_click(cx.listener(
                                |this, _event, _window, cx| {
                                    this.security_toggle_plain_confirm(cx);
                                },
                            )),
                        );
                } else {
                    body = body
                        .child(self.security_dialog_field(
                            "security.dialog.passphrase",
                            strings.export_passphrase,
                            dialog_passphrase(dialog),
                        ))
                        .child(self.security_dialog_field(
                            "security.dialog.confirm",
                            strings.export_confirm,
                            dialog_confirm(dialog),
                        ));
                }
                body = body
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    )
                    .children(saved_to.clone().map(|path| {
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "security.dialog.export.saved".to_owned())
                            .child(SharedString::from(path))
                    }));
                let footer =
                    self.security_dialog_footer(strings.export_choose, Variant::Primary, cx);
                (
                    strings.export_title.to_owned(),
                    Some(strings.export_intro.to_owned()),
                    Icon::Transfer,
                    body,
                    footer,
                )
            }
            SecurityDialog::RotateKey {
                kind,
                hosts,
                retire,
                refusal,
                ..
            } => {
                let mut body = self
                    .security_dialog_body("security.dialog.rotate", cx)
                    .child(self.security_kind_segments(*kind, cx))
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.rotate_new_name,
                        dialog_name(dialog),
                    ))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(strings.rotate_hosts),
                    );
                for (host, chosen) in hosts {
                    let id = *host;
                    let name = self
                        .book
                        .host(*host)
                        .map(|host| host.name().to_owned())
                        .unwrap_or_else(|| host.to_string());
                    body = body.child(security_check(
                        format!("security.dialog.rotate.host.{name}"),
                        SharedString::from(name),
                        *chosen,
                        metrics,
                        cx,
                        move |this, cx| this.security_toggle_rotate_host(id, cx),
                    ));
                }
                body = body.child(
                    checkbox(
                        "security.dialog.rotate.retire",
                        strings.rotate_retire,
                        *retire,
                        metrics,
                    )
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.security_toggle_retire(cx);
                    })),
                );
                if !*retire {
                    body = body.child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.rotate_keep_hint),
                    );
                }
                body = body
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(strings.rotate_intro),
                    );
                let footer = self.security_dialog_footer(strings.rotate, Variant::Primary, cx);
                (
                    strings.rotate_title.to_owned(),
                    Some(strings.rotate_intro.to_owned()),
                    Icon::Refresh,
                    body,
                    footer,
                )
            }
            SecurityDialog::DeleteKey { affected, .. } => {
                let body = self.security_delete_body(
                    "security.dialog.delete-key",
                    strings.delete_key_title,
                    strings.delete_affected,
                    strings.delete_none_affected,
                    affected,
                    cx,
                );
                let footer = self.security_dialog_footer(strings.delete, Variant::Danger, cx);
                (
                    strings.delete_key_title.to_owned(),
                    None,
                    Icon::Trash,
                    body,
                    footer,
                )
            }
            SecurityDialog::NewCredential {
                letters,
                digits,
                symbols,
                refusal,
                ..
            } => {
                let mut body = self
                    .security_dialog_body("security.dialog.new-credential", cx)
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.credential_name,
                        dialog_name(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.user",
                        strings.credential_user,
                        dialog_user(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.secret",
                        strings.credential_password,
                        dialog_secret(dialog),
                    ));
                for (which, label, checked) in [
                    (0_u8, strings.charset_letters, *letters),
                    (1, strings.charset_digits, *digits),
                    (2, strings.charset_symbols, *symbols),
                ] {
                    let selector = format!("security.dialog.charset.{which}");
                    body = body.child(security_check(
                        selector,
                        label,
                        checked,
                        metrics,
                        cx,
                        move |this, cx| this.security_toggle_charset(which, cx),
                    ));
                }
                body = body
                    .child(self.security_dialog_field(
                        "security.dialog.length",
                        strings.password_length,
                        dialog_length(dialog),
                    ))
                    .child(
                        components::compact_button(
                            strings.generate_password,
                            Variant::Secondary,
                            metrics,
                        )
                        .id("security.dialog.generate.password")
                        .debug_selector(|| "security.dialog.generate.password".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_generate_password(cx);
                            },
                        )),
                    )
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    );
                let footer = self.security_dialog_footer(strings.save, Variant::Primary, cx);
                (
                    strings.new_credential_title.to_owned(),
                    None,
                    Icon::Lock,
                    body,
                    footer,
                )
            }
            SecurityDialog::EditCredential {
                letters,
                digits,
                symbols,
                refusal,
                ..
            } => {
                let mut body = self
                    .security_dialog_body("security.dialog.edit-credential", cx)
                    .child(self.security_dialog_field(
                        "security.dialog.name",
                        strings.credential_name,
                        dialog_name(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.user",
                        strings.credential_user,
                        dialog_user(dialog),
                    ))
                    .child(self.security_dialog_field(
                        "security.dialog.secret",
                        strings.edit_credential_password,
                        dialog_secret(dialog),
                    ));
                for (which, label, checked) in [
                    (0_u8, strings.charset_letters, *letters),
                    (1, strings.charset_digits, *digits),
                    (2, strings.charset_symbols, *symbols),
                ] {
                    let selector = format!("security.dialog.charset.{which}");
                    body = body.child(security_check(
                        selector,
                        label,
                        checked,
                        metrics,
                        cx,
                        move |this, cx| this.security_toggle_charset(which, cx),
                    ));
                }
                body = body
                    .child(self.security_dialog_field(
                        "security.dialog.length",
                        strings.password_length,
                        dialog_length(dialog),
                    ))
                    .child(
                        components::compact_button(
                            strings.generate_password,
                            Variant::Secondary,
                            metrics,
                        )
                        .id("security.dialog.generate.password")
                        .debug_selector(|| "security.dialog.generate.password".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.security_generate_password(cx);
                            },
                        )),
                    )
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    );
                let footer = self.security_dialog_footer(strings.save, Variant::Primary, cx);
                (
                    strings.edit_credential_title.to_owned(),
                    None,
                    Icon::Pencil,
                    body,
                    footer,
                )
            }
            SecurityDialog::DeleteCredential { affected, .. } => {
                let body = self.security_delete_body(
                    "security.dialog.delete-credential",
                    strings.delete_credential_title,
                    strings.delete_affected,
                    strings.delete_none_affected,
                    affected,
                    cx,
                );
                let footer = self.security_dialog_footer(strings.delete, Variant::Danger, cx);
                (
                    strings.delete_credential_title.to_owned(),
                    None,
                    Icon::Trash,
                    body,
                    footer,
                )
            }
            SecurityDialog::DeleteTrust { host, port } => {
                let server = format!("{host}:{port}");
                let metrics = self.metrics();
                let entry = self.effective_trust().entry(host, *port).cloned();
                let mut body = self
                    .security_dialog_body("security.dialog.delete-trust", cx)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "security.dialog.trust.body".to_owned())
                            .child(SharedString::from(copy::fill(
                                strings.trust_delete_body,
                                &[("server", &server)],
                            ))),
                    );
                if let Some(entry) = entry {
                    // The old identity stays fully in sight to the end: what
                    // the confirmation removes is exactly this fingerprint
                    // (R7), not "whatever the server presents next".
                    body = body.child(
                        div()
                            .min_w_0()
                            .max_w(metrics.scale(PUBLIC_KEY_MAX_WIDTH))
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .debug_selector(|| "security.dialog.trust.fingerprint".to_owned())
                            .child(SharedString::from(format!(
                                "{} \u{b7} {}",
                                entry.algorithm(),
                                entry.fingerprint()
                            ))),
                    );
                }
                let footer =
                    self.security_dialog_footer(strings.trust_delete_action, Variant::Danger, cx);
                (
                    strings.trust_delete_title.to_owned(),
                    Some(server),
                    Icon::Trash,
                    body,
                    footer,
                )
            }
            SecurityDialog::BiometricRisk => {
                let metrics = self.metrics();
                let body = self
                    .security_dialog_body("security.dialog.biometric-risk", cx)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "security.dialog.biometric-risk.body".to_owned())
                            .child(strings.biometric_risk_body),
                    );
                let footer = self.security_dialog_footer(
                    strings.biometric_risk_confirm,
                    Variant::Primary,
                    cx,
                );
                (
                    strings.biometric_risk_title.to_owned(),
                    None,
                    Icon::Lock,
                    body,
                    footer,
                )
            }
        };

        let (header, subtitle) = self.modal_header(
            Some(icon),
            title,
            intro,
            "security.dialog.close",
            cx,
            |this, cx| this.security_close_dialog(cx),
        );
        Some(self.modal_with(
            window,
            "security.dialog",
            DIALOG_WIDTH,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
            ModalBehaviour {
                scroll: Some(self.security.dialog_scroll.clone()),
                dismiss: None,
                ..Default::default()
            },
        ))
    }

    /// The scrolling column every dialog body shares, with Escape as the way
    /// out that changes nothing.
    fn security_dialog_body(&self, selector: &'static str, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .py(metrics.scale(spacing::LOOSE))
            .debug_selector(move || selector.to_owned())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.security_close_dialog(cx);
                }
            }))
    }

    /// One labelled field of a dialog, under a selector the tests can type into.
    fn security_dialog_field(
        &self,
        selector: &'static str,
        label: &'static str,
        field: Option<&Entity<TextField>>,
    ) -> Div {
        let metrics = self.metrics();
        let mut cell = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(components::form_label(label, metrics));
        if let Some(field) = field {
            // The selector names the field itself, not its label, so a click
            // through it lands in the text the way the host form's fields do.
            cell = cell.child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || selector.to_owned())
                    .child(field.clone()),
            );
        }
        cell
    }

    /// The chooser of the three kinds this build generates.
    fn security_kind_segments(
        &self,
        chosen: keys_api::ManagedKeyKind,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let mut row =
            components::segmented(metrics).debug_selector(|| "security.dialog.kind".to_owned());
        for (kind, label) in [
            (keys_api::ManagedKeyKind::Ed25519, "Ed25519"),
            (keys_api::ManagedKeyKind::Rsa3072, "RSA 3072"),
            (keys_api::ManagedKeyKind::Rsa4096, "RSA 4096"),
        ] {
            let selector = format!("security.dialog.kind.{label}");
            row = row.child(
                components::segment(label, kind == chosen, metrics)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector({
                        let selector = selector.clone();
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.security_set_generate_kind(kind, cx);
                    })),
            );
        }
        row
    }

    /// The body of a delete question: the connections that would lose their
    /// sign-in, which is what the confirmation is about (R1).
    fn security_delete_body(
        &self,
        selector: &'static str,
        _title: &'static str,
        affected_label: &'static str,
        none_label: &'static str,
        affected: &[HostId],
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let mut body = self.security_dialog_body(selector, cx);
        if affected.is_empty() {
            return body.child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(none_label),
            );
        }
        body = body.child(
            div()
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(affected_label),
        );
        for host in affected {
            let name = self
                .book
                .connection(*host)
                .map(|record| record.name().to_owned())
                .unwrap_or_else(|| host.to_string());
            body = body.child(
                div()
                    .px(metrics.scale(spacing::TIGHT))
                    .py(metrics.scale(spacing::HAIRLINE))
                    .rounded(px(radius::SMALL))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(name)),
            );
        }
        body
    }

    /// The actions of a dialog: leave it, or run what it describes.
    fn security_dialog_footer(
        &self,
        primary: &'static str,
        variant: Variant,
        cx: &mut Context<Self>,
    ) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        components::modal_footer(metrics)
            .child(
                self.text_button(
                    SharedString::from("security.dialog.cancel"),
                    strings.cancel,
                    Variant::Ghost,
                    cx,
                    |this, cx| this.security_close_dialog(cx),
                )
                .h(metrics.control_height()),
            )
            .child(
                self.text_button(
                    SharedString::from("security.dialog.submit"),
                    primary,
                    variant,
                    cx,
                    |this, cx| this.security_submit(cx),
                )
                .h(metrics.control_height()),
            )
    }
}

// ---------------------------------------------------------------------------
// Remembered host passwords (R1, R2, R3, ADR-4, ADR-5)
// ---------------------------------------------------------------------------

impl ShelluneView {
    /// Whether the vault's records can be read now: not locked, and not still
    /// being opened at start-up. A machine without a vault file is ready - it
    /// simply holds nothing.
    pub(crate) fn vault_content_ready(&self) -> bool {
        !self.security.lock.locked && !(self.security.content.is_none() && self.security.warming)
    }

    /// Holds `step` until the vault's records can be read: a locked vault asks
    /// for the one unlock first, a vault still opening is waited for. Returns
    /// whether the step waits (it then runs, or fails, on its own).
    pub(crate) fn security_wait_for_vault(
        &mut self,
        step: SecurityPending,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.security.lock.locked {
            let waiting = self.security_begin(vec![step.clone()], cx);
            if waiting && !self.security.lock.pending.contains(&step) {
                // Another prompt holds the gate; this request is refused, and
                // says so where it was made.
                let reason = self.security_strings().lock_busy.to_owned();
                if step == SecurityPending::HostSecrets {
                    self.host_form_save_failed(&reason);
                }
            }
            return waiting;
        }
        if self.security.content.is_none() && self.security.warming {
            if !self.security.lock.pending.contains(&step) {
                self.security.lock.pending.push(step);
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Opens a vault file this window has not read yet - written by another
    /// writer since the start, with no lock engaged - and holds `step` until
    /// it is open. Returns whether the step waits.
    pub(crate) fn vault_open_for(&mut self, step: SecurityPending, cx: &mut Context<Self>) -> bool {
        if self.security.lock.locked
            || self.security.content.is_some()
            || self.security.warming
            || self.security.open_problem.is_some()
            || !VaultStore::at(self.store.root()).exists()
        {
            return false;
        }
        if !self.security.lock.pending.contains(&step) {
            self.security.lock.pending.push(step);
        }
        self.vault_open_quietly(cx);
        cx.notify();
        true
    }

    /// The password one record remembers, for the connection about to use it
    /// (R2). The one place a remembered password is read:
    ///
    /// - a record that does not remember one has none;
    /// - a change the window has not written into the vault yet answers first;
    /// - the vault answers when it holds the password, and a deleted one is
    ///   not looked for anywhere else - the system store is not asked;
    /// - a vault that holds no record of the host (or no vault at all) has the
    ///   host's old `host-<uuid>` entry read once and migrated (R3);
    /// - a vault that cannot be read now (locked, still opening) answers
    ///   nothing, and one that could not be opened answers its reason, never
    ///   an empty password.
    pub(crate) fn remembered_secret(
        &mut self,
        id: HostId,
        remembers: bool,
        cx: &mut Context<Self>,
    ) -> Result<Option<SecretString>, String> {
        if !remembers {
            return Ok(None);
        }
        // A locked window answers no password, not even one still waiting for
        // the vault: the lock governs every secret read (R2).
        let waiting = if self.security.lock.locked {
            None
        } else {
            self.security.pending_host_secrets.get(&id).or_else(|| {
                self.security
                    .host_job
                    .as_ref()
                    .and_then(|job| job.changes.get(&id))
            })
        };
        if let Some(change) = waiting {
            return Ok(match change {
                PendingHostSecret::Set(secret) => Some(secret.clone()),
                PendingHostSecret::Remove => None,
            });
        }
        match self.security.content.as_ref() {
            Some(content) => match content.host_secret_state(id) {
                HostSecretState::Live => return Ok(content.host_secret(id).cloned()),
                HostSecretState::Tombstoned => return Ok(None),
                HostSecretState::Absent => {}
            },
            None => {
                if let Some(problem) = &self.security.open_problem {
                    return Err(problem.clone());
                }
                if self.security.lock.locked
                    || self.security.warming
                    || VaultStore::at(self.store.root()).exists()
                {
                    return Ok(None);
                }
            }
        }
        Ok(self.host_secret_migrate(id, cx))
    }

    /// Reads one host's old entry for its migration into the vault (R3): the
    /// password is used at once and queued for the vault; the entry is deleted
    /// only after the vault holds it. A read that fails or is refused is "not
    /// saved" for now, and the entry is kept for the next use.
    fn host_secret_migrate(&mut self, id: HostId, cx: &mut Context<Self>) -> Option<SecretString> {
        match self.secrets.load_legacy_host(id) {
            Ok(Some(secret)) => {
                self.security
                    .pending_host_secrets
                    .insert(id, PendingHostSecret::Set(secret.clone()));
                self.security.legacy_forget.insert(id);
                self.host_secrets_flush(cx);
                Some(secret)
            }
            // Nothing saved, or a read that failed or was refused: the entry,
            // if any, is kept and read again next time.
            Ok(None) | Err(_) => None,
        }
    }

    /// Whether a form save is waiting for its password to be in the vault.
    pub(crate) fn host_form_save_pending(&self) -> bool {
        self.security.host_form_save.is_some()
            || self
                .security
                .host_job
                .as_ref()
                .is_some_and(|job| job.form.is_some())
    }

    /// Whether a form save is on its way to the vault and will finish without
    /// the user: a close waits for it, so the record its password belongs to is
    /// written first. One waiting for an unlock is the user's to finish.
    pub(crate) fn host_form_save_in_flight(&self) -> bool {
        self.security
            .host_job
            .as_ref()
            .is_some_and(|job| job.form.is_some())
            || (self.security.host_form_save.is_some() && !self.security.lock.locked)
    }

    /// Saves a form whose record remembers a newly typed password (R1): the
    /// password goes into the vault first, and the record is written only once
    /// the vault holds it. A locked vault is unlocked first.
    pub(crate) fn begin_host_form_save(
        &mut self,
        save: HostFormSave,
        secret: SecretString,
        cx: &mut Context<Self>,
    ) {
        let id = save.edit.record().id();
        // A host this machine remembered before the vault held its password
        // may still have its old entry: it is deleted once the vault holds the
        // new one, never before (R1).
        let unmigrated = self
            .book
            .connection(id)
            .is_some_and(|record| record.remembers_secret())
            && self
                .security
                .content
                .as_ref()
                .is_none_or(|content| content.host_secret_state(id) != HostSecretState::Live);
        if unmigrated {
            self.security.legacy_forget.insert(id);
        }
        self.security
            .pending_host_secrets
            .insert(id, PendingHostSecret::Set(secret));
        self.security.host_form_save = Some(save);
        if self.security_wait_for_vault(SecurityPending::HostSecrets, cx) {
            return;
        }
        self.host_secrets_flush(cx);
        cx.notify();
    }

    /// Ends a form save that could not reach the vault: the form stays open
    /// with the reason, and nothing was written.
    pub(crate) fn host_form_save_failed(&mut self, reason: &str) {
        if let Some(form) = self.security.host_form_save.take() {
            let id = form.edit.record().id();
            if matches!(
                self.security.pending_host_secrets.get(&id),
                Some(PendingHostSecret::Set(_))
            ) {
                self.security.pending_host_secrets.remove(&id);
            }
        }
        self.host_form_message(reason);
    }

    fn host_form_message(&mut self, reason: &str) {
        let message = format!("{}: {reason}", self.strings().host_secret_failed);
        self.hosts.set_message(message, true);
    }

    /// The passwords of records the book no longer remembers - deleted, or no
    /// longer remembering - leave the vault (R1). The old entry of each is
    /// deleted as well, whatever the vault holds: a password re-saved, synced
    /// or imported into the vault does not prove the old entry is gone, and
    /// this is the user's own action, asked for once per host.
    pub(crate) fn host_secrets_forget(&mut self, ids: &[HostId], cx: &mut Context<Self>) {
        let vault_known =
            self.security.content.is_some() || self.security.lock.locked || self.security.warming;
        for id in ids {
            let state = self
                .security
                .content
                .as_ref()
                .map(|content| content.host_secret_state(*id));
            let pending = self.security.pending_host_secrets.contains_key(id)
                || self
                    .security
                    .host_job
                    .as_ref()
                    .is_some_and(|job| job.changes.contains_key(id));
            let recorded =
                matches!(state, Some(HostSecretState::Live)) || (state.is_none() && vault_known);
            if recorded || pending {
                self.security
                    .pending_host_secrets
                    .insert(*id, PendingHostSecret::Remove);
            }
        }
        // A store that cannot be reached leaves an entry nobody reads, which
        // is not a reason to keep the record.
        self.host_legacy_forget(ids.to_vec(), cx);
        self.host_secrets_flush(cx);
    }

    /// Starts the one job that writes every waiting host password change, when
    /// the vault is free and open. A busy vault takes them after its job, a
    /// locked one after its unlock.
    pub(crate) fn host_secrets_flush(&mut self, cx: &mut Context<Self>) {
        if self.security.host_job.is_some() || self.security.busy() || self.security.lock.locked {
            return;
        }
        if self.security.pending_host_secrets.is_empty() && self.security.host_form_save.is_none() {
            return;
        }
        let form = self.security.host_form_save.take();
        let mut changes = std::mem::take(&mut self.security.pending_host_secrets);
        if form.is_none() && !VaultStore::at(self.store.root()).exists() {
            // Without a vault a deletion has nothing to delete; only a
            // password to keep creates one.
            changes.retain(|_, change| matches!(change, PendingHostSecret::Set(_)));
            if changes.is_empty() {
                return;
            }
        }
        let device = self.security.device.clone();
        let records: Vec<ConnectionRecord> = form
            .as_ref()
            .map(|form| form.edit.record())
            .into_iter()
            .collect();
        let writes = changes.clone();
        let edit: SecurityEdit = Box::new(move |content, _hosts, _connections| {
            for (id, change) in writes {
                match change {
                    PendingHostSecret::Set(secret) => {
                        content.set_host_secret(id, secret, &device);
                    }
                    PendingHostSecret::Remove => {
                        content.remove_host_secret(id);
                    }
                }
            }
            Ok(SecurityChange::Save {
                report: SecurityReport::HostSecretsSaved,
                records,
            })
        });
        self.security.host_job = Some(HostSecretsJob { changes, form });
        self.security_run_quietly(edit, cx);
    }

    /// Takes in what one host password job did. A form that waited writes its
    /// record now; a record that cannot be written puts the vault back, so the
    /// vault and the host file never disagree (R1). Old entries whose
    /// passwords the vault now holds are deleted (R3).
    fn host_secrets_finished(
        &mut self,
        job: HostSecretsJob,
        result: SecurityResult,
        cx: &mut Context<Self>,
    ) {
        self.security.job_book = None;
        let had_form = job.form.is_some();
        let mut form_saved = false;
        match result {
            SecurityResult::Applied {
                content, previous, ..
            } => {
                let migrated: Vec<HostId> = self
                    .security
                    .legacy_forget
                    .iter()
                    .copied()
                    .filter(|id| content.host_secret_state(*id) == HostSecretState::Live)
                    .collect();
                if !self.security.lock.locked {
                    self.security.content = Some(content);
                }
                let written = match job.form {
                    Some(form) => self.finish_host_form_save(form, cx),
                    None => Ok(()),
                };
                match written {
                    Ok(()) => {
                        form_saved = true;
                        self.host_legacy_forget(migrated, cx);
                    }
                    Err(reason) => {
                        self.security_rollback(*previous, cx);
                        self.host_form_message(&reason);
                    }
                }
            }
            SecurityResult::Refused(reason) | SecurityResult::Failed(reason) => {
                // Nothing was written: a migrated password stays in its old
                // entry, and a form keeps what was typed.
                if had_form {
                    self.host_form_message(&reason);
                } else {
                    self.security_set_notice(
                        Tone::Danger,
                        format!("{}: {reason}", self.security_strings().vault_failed),
                        cx,
                    );
                }
            }
        }
        if had_form {
            // A close that waited for this form goes now, or is called off
            // so the reason can be read.
            self.exit_after_form_save(form_saved, cx);
        }
        cx.notify();
    }

    /// What runs once a vault job ended: a backup import that waited for it
    /// first, else the host passwords that waited, else a sync that waited.
    fn security_job_idle(&mut self, cx: &mut Context<Self>) {
        if !self.security.busy
            && let Some(task) = self.security.import_waiting.take()
        {
            task(self, cx);
            return;
        }
        self.host_secrets_flush(cx);
        if !self.security.busy {
            self.resume_sync_after_vault_job(cx);
        }
    }

    /// Marks a sync job as running, or ends the mark: while it holds, no vault
    /// job starts. Ending it does not flush the host passwords that waited;
    /// the sync's finish does, after its vault re-read started, so they are
    /// written onto what the sync left (R4).
    pub(crate) fn security_set_sync_writing(&mut self, writing: bool) {
        self.security.sync_writing = writing;
    }

    /// Runs `task` - a backup import that writes the vault file itself - once
    /// no vault job runs: now, or when the running one ends. A later request
    /// replaces one that still waits. Returns whether it waits.
    pub(crate) fn security_when_no_job(
        &mut self,
        task: VaultWriterTask,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.security.busy {
            self.security.import_waiting = Some(task);
            return true;
        }
        task(self, cx);
        false
    }

    /// Marks a backup import as writing the vault, or ends the mark: while
    /// it holds, no vault job starts, and the host passwords that waited are
    /// written once it ends.
    pub(crate) fn security_set_import_writing(&mut self, writing: bool, cx: &mut Context<Self>) {
        self.security.import_writing = writing;
        if !writing {
            self.host_secrets_flush(cx);
        }
    }

    /// Whether a vault job runs now (the start-up open does not count).
    pub(crate) fn security_job_running(&self) -> bool {
        self.security.busy
    }

    /// Why the vault's records cannot be read now, when they cannot: what a
    /// writer that needs them (a backup export sealing host passwords) says
    /// instead of reading the file behind the lock. `None` when they can, or
    /// when this machine has no vault.
    pub(crate) fn vault_unreadable_reason(&self) -> Option<String> {
        if self.security.content.is_some() || !VaultStore::at(self.store.root()).exists() {
            return None;
        }
        Some(
            self.security
                .open_problem
                .clone()
                .unwrap_or_else(|| self.security_strings().lock_state_locked.to_owned()),
        )
    }

    /// Deletes the old entries of hosts the saved vault now holds, after any
    /// successful job: a deletion that failed earlier is tried again here.
    fn host_legacy_cleanup(&mut self, content: &VaultContent, cx: &mut Context<Self>) {
        let migrated: Vec<HostId> = self
            .security
            .legacy_forget
            .iter()
            .copied()
            .filter(|id| content.host_secret_state(*id) == HostSecretState::Live)
            .collect();
        self.host_legacy_forget(migrated, cx);
    }

    /// Deletes these hosts' old entries off the UI thread (a deletion may wait
    /// on the system's own prompt). One that fails is kept for the next try;
    /// the connection it served was never affected.
    pub(crate) fn host_legacy_forget(&mut self, ids: Vec<HostId>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            self.security.legacy_forget.remove(id);
        }
        let secrets = self.secrets.clone();
        let work = cx.background_executor().spawn(async move {
            ids.into_iter()
                .filter(|id| secrets.forget_legacy_host(*id).is_err())
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let failed = work.await;
            if failed.is_empty() {
                return;
            }
            let _ = this.update(cx, |view, _cx| {
                view.security.legacy_forget.extend(failed);
            });
        })
        .detach();
    }

    /// What a batch run reads remembered passwords from, besides the vault
    /// copy: the changes not written yet, and whether a host the vault does not
    /// know may have its old entry read.
    pub(crate) fn batch_remembered(&self) -> (BTreeMap<HostId, Option<SecretString>>, bool) {
        let mut pending = BTreeMap::new();
        let in_flight = self
            .security
            .host_job
            .iter()
            .flat_map(|job| job.changes.iter());
        for (id, change) in in_flight.chain(self.security.pending_host_secrets.iter()) {
            let answer = match change {
                // A locked window hands out no password, not even one still
                // waiting for the vault.
                PendingHostSecret::Set(_) if self.security.lock.locked => continue,
                PendingHostSecret::Set(secret) => Some(secret.clone()),
                PendingHostSecret::Remove => None,
            };
            pending.insert(*id, answer);
        }
        let legacy = match self.security.content {
            Some(_) => true,
            None => {
                !self.security.lock.locked
                    && !self.security.warming
                    && self.security.open_problem.is_none()
                    && !VaultStore::at(self.store.root()).exists()
            }
        };
        (pending, legacy)
    }

    /// Keeps the passwords a started batch run will read from old entries.
    pub(crate) fn batch_started_with(&mut self, migrations: crate::app::batch::BatchMigrations) {
        self.security.batch_migrations = Some(migrations);
    }

    /// Moves what the ended batch run read from old entries into the vault, as
    /// one job, and deletes those entries once it is saved (R3).
    pub(crate) fn batch_finished_migrations(&mut self, cx: &mut Context<Self>) {
        let Some(migrations) = self.security.batch_migrations.take() else {
            return;
        };
        let read = match migrations.lock() {
            Ok(mut read) => std::mem::take(&mut *read),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for (id, secret) in read {
            let known =
                self.security.content.as_ref().is_some_and(|content| {
                    content.host_secret_state(id) != HostSecretState::Absent
                });
            let remembers = self
                .book
                .connection(id)
                .is_some_and(|record| record.remembers_secret());
            if known || !remembers || self.security.pending_host_secrets.contains_key(&id) {
                continue;
            }
            self.security
                .pending_host_secrets
                .insert(id, PendingHostSecret::Set(secret));
            self.security.legacy_forget.insert(id);
        }
        self.host_secrets_flush(cx);
    }
}
// ---------------------------------------------------------------------------
// What the tests read and drive
// ---------------------------------------------------------------------------

#[cfg(test)]
impl ShelluneView {
    /// Holds (or releases) the finish of the next vault job, so the busy state
    /// the window draws while work is in flight can be asserted.
    pub fn security_hold_finish_for_test(&mut self, hold: bool) {
        self.security.hold_finish = hold;
    }

    /// Lets a held job finish, and applies what it did.
    pub fn security_release_finish_for_test(&mut self, cx: &mut Context<Self>) {
        self.security.hold_finish = false;
        if let Some(result) = self.security.held.take() {
            self.security_finished(result, cx);
        }
    }

    /// Holds (or releases) the finish of the next system verification prompt.
    pub fn security_hold_verify_for_test(&mut self, hold: bool) {
        self.security.hold_verify = hold;
    }

    /// Lets a held system verification finish, applying its answer.
    pub fn security_release_verify_for_test(&mut self, cx: &mut Context<Self>) {
        self.security.hold_verify = false;
        if let Some((epoch, answer)) = self.security.held_verify.take() {
            self.security_verify_finished(epoch, answer, cx);
        }
    }

    /// Holds (or releases) the finish of `vault_open_quietly` for deterministic warming tests.
    pub fn security_hold_open_for_test(&mut self, hold: bool) {
        self.security.hold_open = hold;
    }

    /// Lets a held `vault_open_quietly` finish, applying its result if its epoch is still valid.
    pub fn security_release_open_for_test(&mut self, cx: &mut Context<Self>) {
        self.security.hold_open = false;
        if let Some((epoch, opened)) = self.security.held_open.take() {
            self.vault_open_quietly_finished(epoch, opened, cx);
        }
    }

    /// Returns the vault list scroll handle for tests verifying independent master-detail scroll.
    pub fn security_list_scroll_for_test(&self) -> ScrollHandle {
        self.security.list_scroll.clone()
    }

    /// Returns the name of the currently opened detail record, if any, for tests.
    pub fn security_detail_name_for_test(&self) -> Option<String> {
        let vault = self.security.content.as_ref()?;
        match self.security.detail? {
            SecurityRecord::Key(id) => vault.key(id).map(|r| r.meta().name().to_owned()),
            SecurityRecord::Credential(id) => {
                vault.credential(id).map(|r| r.meta().name().to_owned())
            }
        }
    }

    /// Triggers a background vault re-read (`warming = true`) for tests.
    pub fn security_reread_for_test(&mut self, cx: &mut Context<Self>) {
        self.vault_reread(cx);
    }

    /// Locks the vault immediately for tests.
    pub fn security_lock_now_for_test(&mut self, cx: &mut Context<Self>) {
        self.security_lock_now(cx);
    }

    /// Starts an independent verification prompt (`security_begin_always`) for concurrent-prompt tests.
    pub fn security_begin_always_for_test(&mut self, cx: &mut Context<Self>) {
        self.security_begin_always(Vec::new(), cx);
    }

    /// Reads the open dialog's `name` field text, when present, for tests.
    pub fn security_dialog_name_for_test(&self, cx: &App) -> Option<String> {
        dialog_name(self.security.dialog.as_ref()?).map(|field| field.read(cx).text().to_owned())
    }

    /// Reads the open import dialog's `pem` field text, when present, for tests.
    pub fn security_dialog_pem_for_test(&self, cx: &App) -> Option<String> {
        dialog_pem(self.security.dialog.as_ref()?).map(|field| field.read(cx).text().to_owned())
    }

    /// Whether a vault detail card is currently open.
    pub fn security_detail_open_for_test(&self) -> bool {
        self.security.detail.is_some()
    }

    /// The live keys of the vault: their identity, name and fingerprint.
    pub fn security_keys_for_test(&self) -> Vec<(KeyId, String, String)> {
        self.security
            .content
            .as_ref()
            .map(|vault| {
                vault
                    .keys()
                    .filter(|record| record.meta().is_live())
                    .map(|record| {
                        (
                            record.meta().id(),
                            record.meta().name().to_owned(),
                            record.meta().fingerprint().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The live shared passwords of the vault: their identity and name.
    pub fn security_credentials_for_test(&self) -> Vec<(CredentialId, String)> {
        self.security
            .content
            .as_ref()
            .map(|vault| {
                vault
                    .credentials()
                    .filter(|record| record.meta().is_live())
                    .map(|record| (record.meta().id(), record.meta().name().to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The password of one shared credential, for the assertions that follow a
    /// connection's credential to the value it really signs in with.
    pub fn security_credential_password_for_test(&self, id: CredentialId) -> Option<String> {
        self.security
            .content
            .as_ref()
            .and_then(|vault| vault.credential(id))
            .and_then(|record| record.secret())
            .map(|secret| secret.expose().to_owned())
    }

    /// The one-time private key body a connection would receive, by identity.
    pub fn security_key_body_for_test(&self, id: KeyId) -> Option<String> {
        self.security
            .content
            .as_ref()
            .and_then(|vault| vault.key(id))
            .and_then(|record| record.private_key())
            .map(|secret| secret.expose().to_owned())
    }

    /// The notice of the last operation, and whether it refused or failed.
    pub fn security_notice_for_test(&self) -> Option<(String, bool)> {
        self.security
            .notice
            .as_ref()
            .map(|(tone, text)| (text.clone(), matches!(tone, Tone::Danger | Tone::Warning)))
    }

    /// The refusal drawn inside the open dialog, when one is.
    pub fn security_dialog_refusal_for_test(&self) -> Option<String> {
        match self.security.dialog.as_ref() {
            Some(SecurityDialog::ImportKey { refusal, .. })
            | Some(SecurityDialog::GenerateKey { refusal, .. })
            | Some(SecurityDialog::EditKey { refusal, .. })
            | Some(SecurityDialog::ExportKey { refusal, .. })
            | Some(SecurityDialog::RotateKey { refusal, .. })
            | Some(SecurityDialog::NewCredential { refusal, .. })
            | Some(SecurityDialog::EditCredential { refusal, .. }) => refusal.clone(),
            _ => None,
        }
    }

    /// Whether a vault job is in flight.
    pub fn security_busy_for_test(&self) -> bool {
        self.security.busy()
    }

    /// The unlocked vault itself, for the assertions that follow a connection's
    /// credentials all the way to the body it would sign with.
    pub fn security_content_clone_for_test(&self) -> Option<VaultContent> {
        self.security.content.clone()
    }

    /// Fills the open import dialog the way a paste would, with text a test
    /// cannot type through the keyboard path.
    pub fn security_fill_import_for_test(
        &mut self,
        pem: &str,
        passphrase: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(SecurityDialog::ImportKey {
            pem: pem_field,
            passphrase: pass_field,
            name: name_field,
            ..
        }) = self.security.dialog.as_ref()
        {
            let pem_field = pem_field.clone();
            let pass_field = pass_field.clone();
            let name_field = name_field.clone();
            pem_field.update(cx, |field, cx| field.set_text(pem, cx));
            pass_field.update(cx, |field, cx| field.set_text(passphrase, cx));
            name_field.update(cx, |field, cx| field.set_text(name, cx));
        }
        cx.notify();
    }

    /// Hands the window a scripted verifier, so a test states the answers the
    /// system would give without ever raising a real prompt (R3, R4).
    pub fn security_set_authenticator_for_test(
        &mut self,
        authenticator: std::sync::Arc<dyn BiometricAuthenticator>,
    ) {
        self.security.lock.authenticator = authenticator;
    }

    /// Turns the device's biometric protection on with a policy and an idle
    /// time, the way the lock page does, and locks the vault as a start under
    /// that protection would (R3).
    pub fn security_enable_lock_for_test(
        &mut self,
        policy: LockPolicy,
        idle_minutes: u16,
        cx: &mut Context<Self>,
    ) {
        self.security.lock.settings = SecuritySettings {
            policy,
            idle_lock_minutes: Some(idle_minutes),
            biometric_unlock: Some(true),
        };
        SecurityStore::at(self.store.root())
            .save(&self.security.lock.settings)
            .expect("the lock settings are written");
        self.security_lock_now(cx);
    }

    /// Whether the vault of this device is locked right now.
    pub fn security_locked_for_test(&self) -> bool {
        self.security.lock.locked
    }

    /// Re-runs the start-up decision about the lock, as a relaunch would, so
    /// a test can change the verifier first and watch what a machine like
    /// that would really do (R3, R4).
    pub fn security_relaunch_for_test(&mut self, cx: &mut Context<Self>) {
        self.security.vault_epoch = self.security.vault_epoch.wrapping_add(1);
        self.security.warming = false;
        self.security.content = None;
        self.security.lock.locked = false;
        self.vault_warm(cx);
    }

    /// The biometric switch as it stands on this device.
    pub fn security_biometric_for_test(&self) -> bool {
        self.security.lock.settings.biometric_unlock == Some(true)
    }

    /// Unlocks the vault the way the page's button does: one system
    /// verification, then the file opens.
    pub fn security_unlock_for_test(&mut self, cx: &mut Context<Self>) {
        self.security_begin(Vec::new(), cx);
    }

    /// The rows of the host-trust page: host, port, algorithm, fingerprint and
    /// whether the vault holds the record.
    pub fn security_trust_rows_for_test(
        &self,
        cx: &App,
    ) -> Vec<(String, u16, String, String, bool)> {
        self.trust_rows(cx)
            .into_iter()
            .map(|row| {
                (
                    row.host,
                    row.port,
                    row.algorithm,
                    row.fingerprint,
                    row.from_vault,
                )
            })
            .collect()
    }

    /// The identities the vault itself holds, as host and port.
    pub fn security_vault_trust_for_test(&self) -> Vec<(String, u16)> {
        self.security
            .content
            .as_ref()
            .map(|vault| {
                vault
                    .trust()
                    .entries()
                    .iter()
                    .map(|entry| (entry.host().to_owned(), entry.port()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Changes the lock policy the way the page does, without locking what is
    /// already open: a test of the per-operation gate starts from an unlocked
    /// vault (R3).
    pub fn security_lock_settings_for_test(&mut self, policy: LockPolicy, cx: &mut Context<Self>) {
        self.security.lock.settings.policy = policy;
        self.security.lock.settings.biometric_unlock = Some(true);
        SecurityStore::at(self.store.root())
            .save(&self.security.lock.settings)
            .expect("the lock settings are written");
        cx.notify();
    }

    /// Shows the password of one record behind the lock's gate, as the card's
    /// button does.
    pub fn security_reveal_for_test(&mut self, credential: CredentialId, cx: &mut Context<Self>) {
        self.security_reveal(credential, cx);
    }

    /// Hides the shown password again, as the card's own button does.
    pub fn security_hide_password_for_test(&mut self, cx: &mut Context<Self>) {
        self.security.revealed = false;
        cx.notify();
    }

    /// Whether the detail card is showing a password.
    pub fn security_password_shown_for_test(&self) -> bool {
        self.security.revealed
    }

    /// Whether the one-shot sensitive-operation grant flag is currently set.
    pub fn security_operation_granted_for_test(&self) -> bool {
        self.security.operation_granted
    }

    /// The saved sync passphrase, the one secret a vault backup and a
    /// passwordless WebDAV share (R9).
    pub fn saved_sync_passphrase_for_test(&self) -> Option<SecretString> {
        self.saved_sync_passphrase()
    }
}

// ---------------------------------------------------------------------------
// Field pickers of the dialog, so one draw can name any variant's fields
// without matching the whole enum again.
// ---------------------------------------------------------------------------

/// Everything the open dialog's fields hold, as owned values, so a submit can
/// decide on them without borrowing the dialog it is about to change.
enum DialogSnapshot {
    Import {
        pem: String,
        passphrase: String,
        name: String,
    },
    Generate {
        kind: keys_api::ManagedKeyKind,
        name: String,
        comment: String,
    },
    EditKey {
        key: KeyId,
        name: String,
        comment: String,
    },
    Export,
    Rotate {
        previous: KeyId,
        kind: keys_api::ManagedKeyKind,
        name: String,
        moving: Vec<HostId>,
        retire: bool,
    },
    DeleteKey(KeyId),
    NewCredential {
        name: String,
        user: String,
        password: String,
    },
    EditCredential {
        credential: CredentialId,
        name: String,
        user: String,
        password: String,
    },
    DeleteCredential(CredentialId),
    DeleteTrust {
        host: String,
        port: u16,
    },
    BiometricRisk,
}

/// Reads the open dialog's fields into owned values.
fn security_dialog_snapshot(dialog: Option<&SecurityDialog>, cx: &App) -> Option<DialogSnapshot> {
    let text = |field: &Entity<TextField>| field.read(cx).text().to_owned();
    Some(match dialog? {
        SecurityDialog::ImportKey {
            pem,
            passphrase,
            name,
            ..
        } => DialogSnapshot::Import {
            pem: text(pem),
            passphrase: text(passphrase),
            name: text(name).trim().to_owned(),
        },
        SecurityDialog::GenerateKey {
            kind,
            name,
            comment,
            ..
        } => DialogSnapshot::Generate {
            kind: *kind,
            name: text(name).trim().to_owned(),
            comment: text(comment).trim().to_owned(),
        },
        SecurityDialog::EditKey {
            key, name, comment, ..
        } => DialogSnapshot::EditKey {
            key: *key,
            name: text(name).trim().to_owned(),
            comment: text(comment).trim().to_owned(),
        },
        SecurityDialog::ExportKey { .. } => DialogSnapshot::Export,
        SecurityDialog::RotateKey {
            previous,
            kind,
            name,
            hosts,
            retire,
            ..
        } => DialogSnapshot::Rotate {
            previous: *previous,
            kind: *kind,
            name: text(name).trim().to_owned(),
            moving: hosts
                .iter()
                .filter(|(_, chosen)| *chosen)
                .map(|(host, _)| *host)
                .collect(),
            retire: *retire,
        },
        SecurityDialog::DeleteKey { key, .. } => DialogSnapshot::DeleteKey(*key),
        SecurityDialog::NewCredential {
            name, user, secret, ..
        } => DialogSnapshot::NewCredential {
            name: text(name).trim().to_owned(),
            user: text(user).trim().to_owned(),
            password: text(secret),
        },
        SecurityDialog::EditCredential {
            credential,
            name,
            user,
            secret,
            ..
        } => DialogSnapshot::EditCredential {
            credential: *credential,
            name: text(name).trim().to_owned(),
            user: text(user).trim().to_owned(),
            password: text(secret),
        },
        SecurityDialog::DeleteCredential { credential, .. } => {
            DialogSnapshot::DeleteCredential(*credential)
        }
        SecurityDialog::DeleteTrust { host, port } => DialogSnapshot::DeleteTrust {
            host: host.clone(),
            port: *port,
        },
        SecurityDialog::BiometricRisk => DialogSnapshot::BiometricRisk,
    })
}

fn dialog_pem(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::ImportKey { pem, .. } => Some(pem),
        _ => None,
    }
}

fn dialog_passphrase(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::ImportKey { passphrase, .. } => Some(passphrase),
        SecurityDialog::ExportKey { passphrase, .. } => Some(passphrase),
        _ => None,
    }
}

fn dialog_confirm(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::ExportKey { confirm, .. } => Some(confirm),
        _ => None,
    }
}

fn dialog_name(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::ImportKey { name, .. }
        | SecurityDialog::GenerateKey { name, .. }
        | SecurityDialog::EditKey { name, .. }
        | SecurityDialog::RotateKey { name, .. }
        | SecurityDialog::NewCredential { name, .. }
        | SecurityDialog::EditCredential { name, .. } => Some(name),
        _ => None,
    }
}

fn dialog_comment(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::GenerateKey { comment, .. } | SecurityDialog::EditKey { comment, .. } => {
            Some(comment)
        }
        _ => None,
    }
}

fn dialog_user(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::NewCredential { user, .. }
        | SecurityDialog::EditCredential { user, .. } => Some(user),
        _ => None,
    }
}

fn dialog_secret(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::NewCredential { secret, .. }
        | SecurityDialog::EditCredential { secret, .. } => Some(secret),
        _ => None,
    }
}

fn dialog_length(dialog: &SecurityDialog) -> Option<&Entity<TextField>> {
    match dialog {
        SecurityDialog::NewCredential { length, .. }
        | SecurityDialog::EditCredential { length, .. } => Some(length),
        _ => None,
    }
}

/// The banner a dialog's refusal is drawn as.
fn security_refusal_banner(reason: String, metrics: Metrics) -> Div {
    components::banner(Tone::Warning, metrics)
        .debug_selector(|| "security.dialog.refusal".to_owned())
        .text_color(theme::swatch(palette::TEXT_PRIMARY))
        .child(SharedString::from(reason))
}

/// A checkbox row whose id is built at draw time, for the rows whose set is
/// the data (a rotation's connections, the generator's character classes).
///
/// The same shape as [`components::checkbox`], which asks for a static id.
fn security_check(
    id: String,
    label: impl Into<SharedString>,
    checked: bool,
    metrics: Metrics,
    cx: &mut Context<ShelluneView>,
    on_click: impl Fn(&mut ShelluneView, &mut Context<ShelluneView>) + 'static,
) -> Stateful<Div> {
    let selector = id.clone();
    let hover = theme::swatch(palette::TEXT_PRIMARY);
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        .cursor_pointer()
        .text_size(metrics.label_size())
        .text_color(theme::swatch(if checked {
            palette::TEXT_PRIMARY
        } else {
            palette::TEXT_SECONDARY
        }))
        .hover(move |style| style.text_color(hover))
        .child(components::checkbox_box(checked, metrics))
        .child(label.into())
        .on_click(cx.listener(move |this, _event, _window, cx| on_click(this, cx)))
}

// ---------------------------------------------------------------------------
// The vault job, off the UI thread
// ---------------------------------------------------------------------------

/// The ways a save of this machine's vault keeps itself openable: the local
/// unseal secret, plus every passphrase wrap the file already carries whose
/// secret the system store can still hand over (T5's handoff: an app-side
/// save must not quietly drop the travelling wrap a sync left beside the
/// local one, or the vault becomes a ciphertext only this machine can open).
pub(super) fn vault_unseal_sources(
    store: &VaultStore,
    secrets: &dyn SecretStore,
) -> Vec<UnsealSource> {
    let mut sources = vec![UnsealSource::LocalMachine];
    if let Ok(Some(cipher)) = store.read_cipher() {
        for purpose in cipher.passphrase_purposes() {
            let held = match purpose {
                PassphrasePurpose::WebDavPassword => secrets.load_app(AppSecret::WebDavPassword),
                PassphrasePurpose::SyncPassphrase => secrets.load_app(AppSecret::SyncPassphrase),
            };
            if let Ok(Some(secret)) = held
                && !secret.expose().trim().is_empty()
            {
                sources.push(match purpose {
                    PassphrasePurpose::WebDavPassword => UnsealSource::WebDavPassword(secret),
                    PassphrasePurpose::SyncPassphrase => UnsealSource::SyncPassphrase(secret),
                });
            }
        }
    }
    sources
}

/// Opens the vault of this machine, applies one edit and saves it - all off
/// the UI thread. Nothing partial is ever written: the edit either returned a
/// change, which is saved as a whole, or a refusal, which writes nothing.
fn security_apply(
    store: VaultStore,
    secrets: std::sync::Arc<dyn SecretStore>,
    hosts: Vec<HostConfig>,
    connections: Vec<ConnectionRecord>,
    edit: SecurityEdit,
) -> SecurityResult {
    let mut content = match store.open(&UnsealSource::LocalMachine, secrets.as_ref()) {
        Ok(content) => content,
        // No vault yet: the first record creates one, sealed for this machine
        // only, which is the no-master-password local mode of R2.
        Err(VaultStoreError::Missing { .. }) => VaultContent::new(),
        Err(error) => return SecurityResult::Failed(error.to_string()),
    };
    let previous = Box::new(content.clone());
    match edit(&mut content, &hosts, &connections) {
        Ok(SecurityChange::ReadOnly(report)) => SecurityResult::Applied {
            content,
            previous,
            records: {
                let mut all: Vec<ConnectionRecord> =
                    hosts.into_iter().map(ConnectionRecord::Ssh).collect();
                all.extend(connections);
                all
            },
            changed: Vec::new(),
            report,
        },
        Ok(SecurityChange::Save {
            report,
            records: changed_records,
        }) => {
            let mut all: Vec<ConnectionRecord> =
                hosts.into_iter().map(ConnectionRecord::Ssh).collect();
            all.extend(connections);
            for record in &changed_records {
                if let Some(slot) = all
                    .iter_mut()
                    .find(|candidate| candidate.id() == record.id())
                {
                    *slot = record.clone();
                } else {
                    // A record the edit creates - a new host whose form waits
                    // for its password to be in the vault - is part of the book
                    // the vault is saved against.
                    all.push(record.clone());
                }
            }
            // The backstop of every app-side save (ADR-1): a password the book
            // no longer asks for - its host deleted, or no longer remembering -
            // becomes a tombstone, so the vault never keeps one nobody sees.
            content.prune_host_secrets(&all);
            // The reference snapshot and the catalog checks run inside the
            // save: an edit that removed a live record without unbinding its
            // hosts is refused before any file is touched, while pre-existing
            // orphan references in `hosts.json` (already absent before this
            // edit) do not deadlock creating a new record to heal them (R2).
            let mirror_records = records_for_vault_mirror(&previous, &content, &all);
            content.set_references(&HostReference::snapshot_records(&mirror_records));
            if let Err(error) = store.save_records(
                &content,
                &mirror_records,
                &vault_unseal_sources(&store, secrets.as_ref()),
                KdfParams::default(),
                secrets.as_ref(),
            ) {
                return SecurityResult::Failed(error.to_string());
            }
            let changed = changed_records.iter().map(ConnectionRecord::id).collect();
            SecurityResult::Applied {
                content,
                previous,
                records: all,
                changed,
                report,
            }
        }
        Err(reason) => SecurityResult::Refused(reason),
    }
}

/// Filters pre-existing orphan vault references (already missing in `previous` and still
/// missing in `content`) out of the mirror slice passed to `VaultStore::save_records`, so
/// a user can create/import a replacement key or password to heal a broken connection while
/// still rejecting any edit that deletes a live bound record without unbinding its hosts.
pub(super) fn records_for_vault_mirror(
    previous: &VaultContent,
    content: &VaultContent,
    records: &[ConnectionRecord],
) -> Vec<ConnectionRecord> {
    let key_live = |vault: &VaultContent, id: KeyId| {
        vault.key(id).is_some_and(|record| record.meta().is_live())
    };
    let cred_live = |vault: &VaultContent, id: CredentialId| {
        vault
            .credential(id)
            .is_some_and(|record| record.meta().is_live())
    };
    records
        .iter()
        .cloned()
        .map(|mut record| {
            match &mut record {
                ConnectionRecord::Ssh(host) => {
                    if let Some(id) = host.managed_key_id()
                        && !key_live(previous, id)
                        && !key_live(content, id)
                    {
                        host.set_auth(None);
                    }
                    if let Some(id) = host.credential_id()
                        && !cred_live(previous, id)
                        && !cred_live(content, id)
                    {
                        host.set_auth(None);
                    }
                }
                ConnectionRecord::Rdp(cfg) => {
                    if let Some(id) = cfg.credential_id()
                        && !cred_live(previous, id)
                        && !cred_live(content, id)
                    {
                        cfg.set_credential_id(None);
                    }
                }
                ConnectionRecord::Vnc(cfg) => {
                    if let Some(id) = cfg.credential_id()
                        && !cred_live(previous, id)
                        && !cred_live(content, id)
                    {
                        cfg.set_credential_id(None);
                    }
                }
                ConnectionRecord::Serial(_) => {}
            }
            record
        })
        .collect()
}

/// The localized sentence of one report, and the tone it is drawn in.
fn security_report_text(report: &SecurityReport, strings: &SecurityStrings) -> (Tone, String) {
    match report {
        SecurityReport::KeySaved(_) => (Tone::Positive, strings.key_saved.to_owned()),
        SecurityReport::CredentialSaved(_) => {
            (Tone::Positive, strings.credential_created.to_owned())
        }
        SecurityReport::CredentialEdited => (Tone::Positive, strings.credential_edited.to_owned()),
        SecurityReport::CredentialDuplicated => {
            (Tone::Positive, strings.credential_duplicated.to_owned())
        }
        SecurityReport::KeyDeleted => (Tone::Positive, strings.delete_key_done.to_owned()),
        SecurityReport::CredentialDeleted => {
            (Tone::Positive, strings.delete_credential_done.to_owned())
        }
        SecurityReport::Unbound { host } => (
            Tone::Positive,
            copy::fill(strings.unbind_done, &[("host", host)]),
        ),
        SecurityReport::Rotated { count, .. } => (
            Tone::Positive,
            copy::fill(strings.rotate_done, &[("count", &count.to_string())]),
        ),
        SecurityReport::Exported { path } => (
            Tone::Positive,
            copy::fill(
                strings.export_done_path,
                &[("path", &path.display().to_string())],
            ),
        ),
        SecurityReport::ImportedWithWarning { warning, .. } => (Tone::Warning, warning.clone()),
        SecurityReport::TrustForgotten { server } => (
            Tone::Positive,
            copy::fill(strings.trust_deleted_note, &[("server", server)]),
        ),
        // The mirror runs behind a connection; its own line never displaces
        // what the connection is saying, so it is drawn nowhere on its own.
        SecurityReport::TrustMirrored | SecurityReport::HostSecretsSaved => {
            (Tone::Neutral, String::new())
        }
    }
}

// ---------------------------------------------------------------------------
// The edits themselves, as functions the background job runs
// ---------------------------------------------------------------------------

/// Imports one private key: parsed, decrypted with its own passphrase when it
/// needs one, checked for the RSA floor, deduplicated by its public key and
/// stored as a record the vault protects from here on.
fn security_import(
    content: &mut VaultContent,
    pem: &str,
    passphrase: &str,
    name: &str,
    device: &str,
    strings: &SecurityStrings,
) -> Result<SecurityChange, String> {
    let offered = (!passphrase.is_empty()).then(|| SecretString::new(passphrase));
    let key = keys_api::import_private_key(&SecretString::new(pem), offered.as_ref())
        .map_err(|error| error.to_string())?;
    // The RSA floor this build keeps (the T2 review hand-off): below 2048 bits
    // the key is not imported at all, and the reason names the size.
    if let Some(bits) = key.key_data().rsa().map(|rsa| rsa.key_size())
        && (bits as usize) < RSA_IMPORT_FLOOR_BITS
    {
        return Err(copy::fill(
            strings.import_rsa_weak,
            &[("bits", &bits.to_string())],
        ));
    }
    let public = keys_api::public_data(&key).map_err(|error| error.to_string())?;
    // The same public key twice is the same key twice, whatever file it came
    // in: the existing record is named instead of a second one being made (R1).
    if let Some(existing) = content
        .keys()
        .find(|record| record.meta().is_live() && record.meta().fingerprint() == public.fingerprint)
    {
        return Err(copy::fill(
            strings.import_duplicate,
            &[("name", existing.meta().name())],
        ));
    }
    // The vault keeps the key unencrypted inside its own sealed body; the file
    // it came from is never read again.
    let body = keys_api::export_private_key(&key, None).map_err(|error| error.to_string())?;
    let id = KeyId::new();
    let meta = ManagedKeyMeta::new(
        id,
        name,
        public.algorithm,
        public.authorized_key,
        public.fingerprint,
        None,
        VaultRecordVersion::new(1, device.to_owned()),
    );
    content
        .insert_key(VaultKeyRecord::new(meta, body))
        .map_err(|error| error.to_string())?;
    // An RSA key between the floor and what this build generates is accepted
    // with a warning, so the reader knows a replacement is worth planning.
    let warning = key.key_data().rsa().and_then(|rsa| {
        let bits = rsa.key_size() as usize;
        // The message is composed here because the job is where the size is
        // known; it carries no secret.
        (bits < RSA_GENERATED_BITS)
            .then(|| copy::fill(strings.import_rsa_small, &[("bits", &bits.to_string())]))
    });
    Ok(match warning {
        Some(warning) => SecurityChange::Save {
            report: SecurityReport::ImportedWithWarning { id, warning },
            records: Vec::new(),
        },
        None => SecurityChange::Save {
            report: SecurityReport::KeySaved(id),
            records: Vec::new(),
        },
    })
}

/// Renders text with accent search highlights when `query` is non-empty.
fn security_highlighted(text: &str, query: &str) -> gpui::AnyElement {
    if query.is_empty() {
        SharedString::from(text.to_owned()).into_any_element()
    } else {
        components::highlighted_text(
            text.to_owned(),
            crate::app::palette::highlights(text, query),
        )
        .into_any_element()
    }
}

/// Rotates one key: the whole transaction is checked on a catalog of the
/// content against the full host list first (the T3 contract), and only a
/// rotation that passed as a whole is written into the content and the hosts.
#[allow(clippy::too_many_arguments)]
fn security_rotate(
    content: &mut VaultContent,
    hosts: &[HostConfig],
    previous: KeyId,
    kind: keys_api::ManagedKeyKind,
    name: &str,
    moving: Vec<HostId>,
    retire: bool,
    device: String,
) -> Result<SecurityChange, String> {
    let key = keys_api::generate(kind).map_err(|error| error.to_string())?;
    let public = keys_api::public_data(&key).map_err(|error| error.to_string())?;
    let body = keys_api::export_private_key(&key, None).map_err(|error| error.to_string())?;
    let new_meta = ManagedKeyMeta::new(
        KeyId::new(),
        name,
        public.algorithm.clone(),
        public.authorized_key.clone(),
        public.fingerprint.clone(),
        None,
        VaultRecordVersion::new(1, device),
    );
    // The catalog holds the metadata of every record; the rotation is checked
    // and applied there as one transaction, with the whole host book.
    let mut catalog = content.catalog().map_err(|error| error.to_string())?;
    let mut moved = hosts.to_vec();
    catalog
        .rotate_key(
            shellune_core::KeyRotation {
                new_key: new_meta.clone(),
                previous_key: previous,
                hosts: moving.clone(),
                retire_previous: retire,
            },
            &mut moved,
        )
        .map_err(|error| error.to_string())?;
    // The transaction was accepted as a whole; the same steps are written into
    // the content the vault saves.
    content
        .insert_key(VaultKeyRecord::new(new_meta, body))
        .map_err(|error| error.to_string())?;
    if retire {
        content
            .remove_key(previous)
            .map_err(|error| error.to_string())?;
    }
    // Only the moved records travel back: the finish replays their new
    // reference on the book as it stands then, keeping edits made meanwhile.
    let moved_records: Vec<ConnectionRecord> = moved
        .iter()
        .filter(|host| moving.contains(&host.id()))
        .cloned()
        .map(ConnectionRecord::Ssh)
        .collect();
    Ok(SecurityChange::Save {
        report: SecurityReport::Rotated {
            count: moving.len(),
            public_key: public.authorized_key,
        },
        records: moved_records,
    })
}

/// Deletes one record. Every connection it signed in with loses that choice,
/// so no binding is left that looks usable and points at nothing (R1).
fn security_delete(
    content: &mut VaultContent,
    hosts: &[HostConfig],
    connections: &[ConnectionRecord],
    record: SecurityRecord,
) -> Result<SecurityChange, String> {
    let mut changed: Vec<ConnectionRecord> = Vec::new();
    let report = match record {
        SecurityRecord::Key(id) => {
            if !content.remove_key(id).map_err(|error| error.to_string())? {
                return Err("the vault holds no such key".to_owned());
            }
            for host in hosts {
                if host.managed_key_id() == Some(id) {
                    let mut unbound = host.clone();
                    unbound.set_auth(None);
                    changed.push(ConnectionRecord::Ssh(unbound));
                }
            }
            SecurityReport::KeyDeleted
        }
        SecurityRecord::Credential(id) => {
            if !content
                .remove_credential(id)
                .map_err(|error| error.to_string())?
            {
                return Err("the vault holds no such password".to_owned());
            }
            for host in hosts {
                if host.credential_id() == Some(id) {
                    let mut unbound = host.clone();
                    unbound.set_auth(None);
                    changed.push(ConnectionRecord::Ssh(unbound));
                }
            }
            // A desktop record that names the password loses the reference the
            // same way; its protocol has no method to clear, so the reference
            // itself goes, and the vault's save would refuse the delete if the
            // binding stayed.
            for connection in connections {
                if connection.credential_id() == Some(id) {
                    match connection.clone() {
                        ConnectionRecord::Rdp(mut config) => {
                            config.set_credential_id(None);
                            changed.push(ConnectionRecord::Rdp(config));
                        }
                        ConnectionRecord::Vnc(mut config) => {
                            config.set_credential_id(None);
                            changed.push(ConnectionRecord::Vnc(config));
                        }
                        _ => {}
                    }
                }
            }
            SecurityReport::CredentialDeleted
        }
    };
    Ok(SecurityChange::Save {
        report,
        records: changed,
    })
}

// ---------------------------------------------------------------------------
// Metadata editing: new metadata under the same identity
// ---------------------------------------------------------------------------

/// A key's metadata with its name and comment replaced.
///
/// The identity, the algorithm, the public key and the fingerprint are carried
/// over exactly, which is what "editing the name changes neither the key nor
/// its fingerprint" asks for; the version moves on, because the record changed.
/// The model has one constructor, so the record's life starts a new clock -
/// a limitation of this build's editing, not of its fingerprints.
fn renamed_key_meta(
    meta: &ManagedKeyMeta,
    name: String,
    comment: Option<String>,
) -> ManagedKeyMeta {
    ManagedKeyMeta::new(
        meta.id(),
        name,
        meta.algorithm().to_owned(),
        meta.public_key().to_owned(),
        meta.fingerprint().to_owned(),
        comment,
        VaultRecordVersion::new(
            meta.version().counter() + 1,
            meta.version().device_id().to_owned(),
        ),
    )
}

/// A credential's metadata with its name and user replaced, under the same
/// identity.
fn renamed_credential_meta(
    meta: &CredentialMeta,
    name: String,
    user: Option<String>,
) -> CredentialMeta {
    CredentialMeta::new(
        meta.id(),
        name,
        user,
        VaultRecordVersion::new(
            meta.version().counter() + 1,
            meta.version().device_id().to_owned(),
        ),
    )
}

/// Replaces one key record with the same record under edited metadata. The
/// content is rebuilt record by record, because a vault record's identity is
/// the one thing an edit must not change.
fn replace_key_meta(
    content: &mut VaultContent,
    id: KeyId,
    meta: ManagedKeyMeta,
) -> Result<(), String> {
    let mut rebuilt = VaultContent::new();
    rebuilt.set_references(content.references());
    rebuilt.set_trust(content.trust().clone());
    for record in content.keys() {
        if record.meta().id() == id {
            let body = record
                .private_key()
                .cloned()
                .ok_or_else(|| "the vault holds no such key".to_owned())?;
            rebuilt
                .insert_key(VaultKeyRecord::new(meta.clone(), body))
                .map_err(|error| error.to_string())?;
        } else {
            let body = record
                .private_key()
                .cloned()
                .unwrap_or_else(|| SecretString::new(""));
            rebuilt
                .insert_key(VaultKeyRecord::new(record.meta().clone(), body))
                .map_err(|error| error.to_string())?;
        }
    }
    for record in content.credentials() {
        let secret = record
            .secret()
            .cloned()
            .unwrap_or_else(|| SecretString::new(""));
        rebuilt
            .insert_credential(VaultCredentialRecord::new(record.meta().clone(), secret))
            .map_err(|error| error.to_string())?;
    }
    *content = rebuilt;
    Ok(())
}

/// Replaces one credential record with the same record under edited metadata
/// and, when a new password was typed, that password.
fn replace_credential(
    content: &mut VaultContent,
    id: CredentialId,
    meta: CredentialMeta,
    secret: SecretString,
) -> Result<(), String> {
    let mut rebuilt = VaultContent::new();
    rebuilt.set_references(content.references());
    rebuilt.set_trust(content.trust().clone());
    for record in content.keys() {
        let body = record
            .private_key()
            .cloned()
            .unwrap_or_else(|| SecretString::new(""));
        rebuilt
            .insert_key(VaultKeyRecord::new(record.meta().clone(), body))
            .map_err(|error| error.to_string())?;
    }
    for record in content.credentials() {
        if record.meta().id() == id {
            rebuilt
                .insert_credential(VaultCredentialRecord::new(meta.clone(), secret.clone()))
                .map_err(|error| error.to_string())?;
        } else {
            let body = record
                .secret()
                .cloned()
                .unwrap_or_else(|| SecretString::new(""));
            rebuilt
                .insert_credential(VaultCredentialRecord::new(record.meta().clone(), body))
                .map_err(|error| error.to_string())?;
        }
    }
    *content = rebuilt;
    Ok(())
}

// ---------------------------------------------------------------------------
// The capture fixture
// ---------------------------------------------------------------------------

#[cfg(feature = "ui-capture")]
impl ShelluneView {
    /// Puts fixture records into the page's in-memory vault, so a capture
    /// draws a populated page without a vault file, a system credential store
    /// or a secret.
    ///
    /// The fixture holds public metadata only - names, algorithms,
    /// fingerprints - and nothing here is written anywhere: the shot shows the
    /// lists this build draws, not a vault that exists.
    pub fn capture_security_records(&mut self, keys: usize, credentials: usize) {
        let mut vault = VaultContent::new();
        for index in 0..keys {
            let name = format!("Deploy key {index}");
            let suffix = capture_suffix(index);
            let _ = vault.insert_key(VaultKeyRecord::new(
                ManagedKeyMeta::new(
                    KeyId::new(),
                    name,
                    "ssh-ed25519",
                    format!("ssh-ed25519 AAAA{suffix} deploy@laptop"),
                    format!("SHA256:{suffix}"),
                    Some("deploy@laptop".to_owned()),
                    VaultRecordVersion::new(1, "capture"),
                ),
                SecretString::new(""),
            ));
        }
        for index in 0..credentials {
            let _ = vault.insert_credential(VaultCredentialRecord::new(
                CredentialMeta::new(
                    CredentialId::new(),
                    format!("Prod login {index}"),
                    Some("deploy".to_owned()),
                    VaultRecordVersion::new(1, "capture"),
                ),
                SecretString::new(""),
            ));
        }
        self.security.content = Some(vault);
    }

    /// Puts fixture accepted identities into the page's in-memory vault, so a
    /// capture of the host-trust page shows rows, not an empty list: nothing
    /// is written to disk and no server was ever asked about (R7).
    pub fn capture_trust_entries(&mut self, count: usize) {
        let mut vault = self.security.content.take().unwrap_or_default();
        let mut trust = vault.trust().clone();
        for index in 0..count {
            let suffix = capture_suffix(index);
            let key = HostKey::new(
                "ssh-ed25519".to_owned(),
                format!("SHA256:{suffix}"),
                format!("AAAA{suffix}"),
            )
            .expect("a fixture identity is well formed");
            trust
                .accept(
                    &format!("web-{index:02}.example.com"),
                    22,
                    &key,
                    1_794_816_000,
                )
                .expect("a fixture identity is accepted");
        }
        vault.set_trust(trust);
        self.security.content = Some(vault);
    }

    /// How many rows the host-trust page lists, for a shot's ready check.
    pub fn security_trust_count_for_capture(&self) -> usize {
        self.security
            .content
            .as_ref()
            .map(|vault| vault.trust().entries().len())
            .unwrap_or(0)
    }

    /// Turns the device's biometric protection on for a capture of the lock
    /// page, without a vault to lock: the shot shows the switch, the policy
    /// and the idle setting, and no system prompt is ever raised (R3).
    pub fn capture_lock_engaged(&mut self) {
        self.security.lock.settings.biometric_unlock = Some(true);
        self.security.lock.settings.policy = LockPolicy::UnlockVault;
        self.security.lock.settings.idle_lock_minutes = Some(15);
    }

    /// How many keys the fixture page lists, so a shot's ready check can say
    /// the lists really drew them.
    pub fn security_keys_count_for_capture(&self) -> usize {
        self.security
            .content
            .as_ref()
            .map(|vault| {
                vault
                    .keys()
                    .filter(|record| record.meta().is_live())
                    .count()
            })
            .unwrap_or_default()
    }
}

/// Distinct, stable fingerprint material for one fixture key.
#[cfg(feature = "ui-capture")]
fn capture_suffix(index: usize) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz";
    (0..8)
        .map(|step| alphabet[(index * 7 + step * 3) % alphabet.len()] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// The password generator
// ---------------------------------------------------------------------------

/// Writes one private key export: to a fresh file beside the target, then an
/// atomic rename over it.
///
/// Two properties the plain `fs::write` cannot give. A fresh file is created
/// with the owner's permissions alone (0600), so an export never inherits a
/// looser mode a file already at that path - or the process umask - would
/// leave open; and the rename replaces the target in one step, so a write that
/// fails halfway leaves the previous file exactly as it was instead of a
/// truncated key.
fn write_private_export(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let directory = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let temp = directory.join(format!(
        ".{}.part-{}",
        path.file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .unwrap_or_default(),
        remote_now(),
    ));
    let write = || -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?
                .write_all(bytes)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?
                .write_all(bytes)?;
        }
        Ok(())
    };
    write().map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        error.to_string()
    })?;
    // A rename replaces in one step on the Unix-like platforms; Windows refuses
    // to rename over an existing file, so the target steps aside there first -
    // the window is the platform's own, and the copy that lands is complete.
    if std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temp, path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            error.to_string()
        })?;
    }
    Ok(())
}

/// The generator, under the name the tests reach it by.
#[cfg(test)]
pub(crate) fn generate_password_for_test(
    length: usize,
    letters: bool,
    digits: bool,
    symbols: bool,
) -> String {
    generate_password(length, letters, digits, symbols)
}

/// Generates a password of `length` characters from the chosen classes, with
/// uniform character choice over the operating system's random source.
///
/// The entropy comes from freshly drawn `KeyId` values (UUID v4, which is the
/// OS CSPRNG under `uuid`); the two bytes that carry the version and variant
/// bits are skipped, and each character is chosen by rejection sampling over
/// pairs of the remaining bytes, so every character of the set is equally
/// likely - no modulo bias, no predictable positions (R1).
pub(crate) fn generate_password(
    length: usize,
    letters: bool,
    digits: bool,
    symbols: bool,
) -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const DIGITS: &[u8] = b"0123456789";
    const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{};:,.?/";
    let mut set: Vec<u8> = Vec::new();
    if letters {
        set.extend_from_slice(LETTERS);
    }
    if digits {
        set.extend_from_slice(DIGITS);
    }
    if symbols {
        set.extend_from_slice(SYMBOLS);
    }
    if set.is_empty() {
        // No class chosen is the caller's gap to fix in the dialog; the
        // generator itself refuses to invent a class the user turned off.
        set.extend_from_slice(LETTERS);
    }
    let count = set.len();
    // The largest multiple of the set size a uniform byte pair (a value in
    // 0..65536) can hold: values above it are drawn again, which is what makes
    // each choice uniform over the whole set.
    let limit = (65_536 / count) * count;
    let mut stream = Entropy::new();
    let mut password = String::with_capacity(length);
    while password.len() < length {
        let value = stream.next_uniform(limit);
        password.push(set[value % count] as char);
    }
    password
}

/// A stream of uniform bytes from the CSPRNG behind `KeyId::new`, skipping the
/// version and variant bytes a v4 UUID fixes.
struct Entropy {
    bytes: Vec<u8>,
    at: usize,
}

impl Entropy {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            at: 0,
        }
    }

    /// Refills the buffer with the uniform bytes of fresh random identifiers.
    fn refill(&mut self) {
        self.bytes.clear();
        self.at = 0;
        let id = KeyId::new().as_uuid();
        let raw = id.as_bytes();
        for (index, byte) in raw.iter().enumerate() {
            // Bytes 6 and 8 carry the UUID version and variant bits, which are
            // fixed rather than random; the rest are uniform.
            if index != 6 && index != 8 {
                self.bytes.push(*byte);
            }
        }
    }

    /// One uniform value below `limit`, by rejection sampling on byte pairs.
    fn next_uniform(&mut self, limit: usize) -> usize {
        loop {
            if self.at + 2 > self.bytes.len() {
                self.refill();
            }
            let high = self.bytes[self.at] as usize;
            let low = self.bytes[self.at + 1] as usize;
            self.at += 2;
            let value = high * 256 + low;
            if value < limit {
                return value;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Seconds since the Unix epoch, for the record times the lists show.
fn remote_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// One record time as seconds since the epoch, never negative.
fn unix_of(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}
