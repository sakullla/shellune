//! Backup export and import, from the settings' data category (R5, R6, R9).
//!
//! A backup is the portable data document of the storage layer: the hosts and the other
//! connections, the groups, the forwards, the saved commands, the trusted server keys and
//! the preferences, with the saved passwords sealed under a passphrase only when the
//! user asks for them. This module is the interface around it and nothing more:
//!
//! * export asks whether the passwords go along - and then for a passphrase typed twice -
//!   lets the system's save dialog choose the file, and says what was written, naming the
//!   connections whose password could not be read;
//! * import reads the chosen file, asks for the passphrase when the file holds passwords,
//!   shows what applying it would change, lets the user merge or replace (a replace asks
//!   a second time), applies it and reloads every list and preference on screen.
//!
//! A file that is damaged, from a newer build or locked with another passphrase is
//! refused with its reason before anything is planned, so nothing on this machine is
//! written. Applying takes a snapshot of every data file first and puts back what it
//! wrote when a later step fails; that is the storage layer's promise, and this module
//! only reports it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{App, Context, Div, Entity, FontWeight, SharedString, Stateful, Window, div};
use shellune_core::{HostId, SecretString};
use shellune_storage::{
    ApplyMode, ApplyOutcome, ApplyPlan, Change, ExportOptions, ExportOutcome, HostReference,
    KdfParams, KeySource, LocalData, LocalState, PassphrasePurpose, Platform as DataPlatform,
    PlatformField, PortableError, SecretBundle, ShelluneData, SnippetStore, UnsealSource,
    VaultContent, VaultStore,
};

use super::ShelluneView;
use super::security::SecurityPending;
use super::shell::ModalContent;
use crate::app::components::{self, Variant};
use crate::app::copy::{self, BackupStrings};
use crate::app::icons::Icon;
use crate::app::snippets::SnippetLibrary;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Tone, palette, spacing};

/// The dialog of the backup flow that is open, when one is.
pub(super) enum BackupDialog {
    /// Writing a backup.
    Export(ExportDialog),
    /// Reading one back.
    Import(ImportDialog),
}

/// The export dialog: whether the passwords go along, their passphrase, and what the last
/// export did.
pub(super) struct ExportDialog {
    include_secrets: bool,
    /// The whole vault travels: keys, passwords, metadata, references and
    /// trust, as an encrypted block of a version 2 document (R9).
    include_vault: bool,
    passphrase: Entity<TextField>,
    confirm: Entity<TextField>,
    /// The sync passphrase of the vault export, typed only when no sync
    /// passphrase is saved; the saved one is used without retyping (R9).
    vault_passphrase: Entity<TextField>,
    vault_confirm: Entity<TextField>,
    /// The system's save dialog is open, so a second press does not open another.
    choosing: bool,
    /// The export is being written on the background executor.
    running: bool,
    /// Why the last export did not write the file.
    failure: Option<String>,
    /// What the last export wrote.
    report: Option<ExportReport>,
}

/// What one export wrote.
struct ExportReport {
    path: PathBuf,
    /// How many passwords are in the file; `None` when none were asked for.
    secrets: Option<usize>,
    /// The encrypted vault travelled with it (R9).
    vault: bool,
    /// The connections whose password the credential store could not read.
    unreadable: Vec<String>,
    /// The connections that remember a password the credential store does not have.
    missing: Vec<String>,
}

/// The import dialog: the file and how far reading it got.
pub(super) struct ImportDialog {
    path: PathBuf,
    stage: ImportStage,
}

/// How far an import got.
enum ImportStage {
    /// The file cannot be used; the reason says why. Nothing was written.
    Refused(String),
    /// The file holds sealed passwords, and the passphrase is being asked for.
    Locked {
        document: Box<ShelluneData>,
        field: Entity<TextField>,
        problem: Option<String>,
        /// The passphrase is being tried on the background executor.
        running: bool,
    },
    /// A version 2 file carries the encrypted vault, and the sync passphrase
    /// it was sealed with is being asked for (R9).
    VaultLocked {
        document: Box<ShelluneData>,
        field: Entity<TextField>,
        problem: Option<String>,
        running: bool,
    },
    /// The preview is being applied on the background executor.
    Applying,
    /// What applying the file would change.
    Preview(Box<BackupPreview>),
    /// The file was applied.
    Applied {
        snapshot: Option<PathBuf>,
        changed: bool,
        /// How many passwords were saved to the credential store.
        saved: usize,
        /// The encrypted vault was restored with it (R9).
        vault: bool,
        /// The connections whose password the credential store would not remove.
        not_forgotten: Vec<String>,
    },
}

/// The passwords of a document, as far as this import is concerned.
enum Secrets {
    /// The document carries none.
    NotInFile,
    /// It carries some, and the user chose to go on without them.
    Skipped,
    /// They were opened with the passphrase.
    Opened(SecretBundle),
}

/// A preview: the document, the plan for the chosen mode, and what the user still has to
/// confirm.
struct BackupPreview {
    document: ShelluneData,
    secrets: Secrets,
    mode: ApplyMode,
    plan: ApplyPlan,
    missing_keys: Vec<MissingKey>,
    /// The vault the document carried, opened with the sync passphrase: it is
    /// restored as one piece with the document, secrets, metadata, references
    /// and trust together (R9). The passphrase is kept for the save's wrap.
    vault: Option<BackupVault>,
    /// The names of the records on this machine when the plan was made, for the
    /// passwords a plan forgets: those records may be gone from the result.
    local_names: HashMap<HostId, String>,
    /// A replace was asked for once and waits for its confirmation.
    confirm_replace: bool,
    /// Why the last apply did not happen.
    problem: Option<String>,
}

/// The opened vault of a version 2 backup and the passphrase that opened it.
struct BackupVault {
    content: VaultContent,
    passphrase: SecretString,
}

/// What applying did to the vault a version 2 backup carried.
enum BackupVaultOutcome {
    /// The backup carried none; nothing vault-shaped was written.
    NotInFile,
    /// The vault was restored as one piece with the document (R9). The
    /// content is carried for the reader that wants what landed; the window
    /// re-reads the file it produced instead of trusting a copy.
    Restored,
    /// The document applied but the vault could not be written; the last good
    /// vault file is untouched and the reason is shown.
    Failed(String),
    /// The backup's sealed host passwords were saved into this machine's vault
    /// with the document (R4); the window re-reads the vault file.
    PasswordsSaved,
}

/// Applies a plan that brings host passwords: the files are written and the
/// passwords set in this machine's vault (created when there is none), saved as
/// one step - a vault that cannot be saved puts the files back (R4, ADR-6).
fn apply_into_vault(
    preview: &BackupPreview,
    data: &LocalData,
    root: &std::path::Path,
    device: &str,
    reason: &str,
    secrets: &dyn shellune_storage::SecretStore,
) -> (Result<ApplyOutcome, PortableError>, BackupVaultOutcome) {
    let store = VaultStore::at(root);
    let mut content = match store.open(&UnsealSource::LocalMachine, secrets) {
        Ok(content) => content,
        Err(shellune_storage::VaultStoreError::Missing { .. }) => VaultContent::new(),
        Err(error) => {
            return (
                Err(PortableError::Local {
                    label: "vault",
                    reason: error.to_string(),
                }),
                BackupVaultOutcome::NotInFile,
            );
        }
    };
    let previous = content.clone();
    let records = preview.plan.result().hosts.records();
    let sources = super::security::vault_unseal_sources(&store, secrets);
    let mut commit = |written: &VaultContent| {
        let mut written = written.clone();
        let mirror = super::security::records_for_vault_mirror(&previous, &written, &records);
        written.set_references(&HostReference::snapshot_records(&mirror));
        written.prune_host_secrets(&records);
        store
            .save_records(&written, &mirror, &sources, KdfParams::default(), secrets)
            .map_err(|error| error.to_string())
    };
    match preview
        .plan
        .apply_with_vault(data, &mut content, device, reason, &mut commit)
    {
        Ok(outcome) => (Ok(outcome), BackupVaultOutcome::PasswordsSaved),
        Err(error) => (Err(error), BackupVaultOutcome::NotInFile),
    }
}

/// Applies a version 2 backup: the files are written, then the backup's vault
/// is saved as one piece with this machine's host passwords the backup has no
/// record of (see [`restored_vault`]), sealed for this machine and for the
/// passphrase that opened it (R4, R9). The plan marks the records whose
/// password the backup's vault holds as remembering it, and gives a tombstone
/// to those that stopped. A vault that cannot be saved leaves the files
/// applied and says why, as the restore always did.
#[allow(clippy::too_many_arguments)]
fn restore_into_vault(
    preview: &BackupPreview,
    vault: &BackupVault,
    data: &LocalData,
    root: &std::path::Path,
    device: &str,
    reason: &str,
    secrets: &dyn shellune_storage::SecretStore,
    local_vault: Option<VaultContent>,
) -> (Result<ApplyOutcome, PortableError>, BackupVaultOutcome) {
    let store = VaultStore::at(root);
    let records = preview.plan.result().hosts.records();
    // This machine's vault as it stands: the window's open copy, else the
    // file. One that exists but cannot be read fails the restore before any
    // file is written: treating it as absent would drop the host passwords
    // only this machine holds (R4).
    let local = match local_vault {
        Some(content) => Some(content),
        None => match store.open(&UnsealSource::LocalMachine, secrets) {
            Ok(content) => Some(content),
            Err(shellune_storage::VaultStoreError::Missing { .. }) => None,
            Err(error) => {
                return (
                    Err(PortableError::Local {
                        label: "vault",
                        reason: error.to_string(),
                    }),
                    BackupVaultOutcome::NotInFile,
                );
            }
        },
    };
    let mut content = restored_vault(&vault.content, local.as_ref(), &records);
    let sources = [
        UnsealSource::LocalMachine,
        UnsealSource::SyncPassphrase(vault.passphrase.clone()),
    ];
    let mut saved: Option<Result<(), String>> = None;
    let outcome = {
        let mut commit = |written: &VaultContent| {
            let mut written = written.clone();
            written.set_references(&HostReference::snapshot_records(&records));
            written.prune_host_secrets(&records);
            saved = Some(
                store
                    .save_records(&written, &records, &sources, backup_kdf(), secrets)
                    .map_err(|error| error.to_string()),
            );
            // The files stay applied whatever the vault did; the outcome
            // below says what that was.
            Ok(())
        };
        preview
            .plan
            .apply_with_vault(data, &mut content, device, reason, &mut commit)
    };
    let vault_outcome = match (&outcome, saved) {
        // The window re-reads the file this wrote, so the outcome carries the
        // fact and not a second copy of the secrets.
        (Ok(_), Some(Ok(()))) => BackupVaultOutcome::Restored,
        (Ok(_), Some(Err(reason))) => BackupVaultOutcome::Failed(reason),
        _ => BackupVaultOutcome::NotInFile,
    };
    (outcome, vault_outcome)
}

/// The vault a version 2 backup restores, against the book the apply ends
/// with: the backup's vault whole, plus every host password record of this
/// machine's vault the backup holds no record of, live or deleted (R4).
///
/// A backup made where a host was not migrated yet says nothing about that
/// host's password; that is not a deletion. Merging keeps the passwords of
/// hosts only this machine has, and replacing keeps those of hosts the result
/// still remembers - otherwise a host would stay marked "remembered" with its
/// password nowhere, its old entry already deleted by the migration. What the
/// result no longer remembers becomes a tombstone, so a sync sees a deletion
/// and not an absence. Where both sides hold a record the backup's wins, the
/// same rule the merge applies to the host records themselves.
fn restored_vault(
    backup: &VaultContent,
    local: Option<&VaultContent>,
    records: &[shellune_core::ConnectionRecord],
) -> VaultContent {
    let mut content = backup.clone();
    if let Some(local) = local {
        for record in local.host_secret_records() {
            if content.host_secret_record(record.meta().host()).is_none() {
                // Absent from the backup, so the insertion cannot collide.
                let _ = content.insert_host_secret(record.clone());
            }
        }
    }
    content.set_references(&HostReference::snapshot_records(records));
    content.prune_host_secrets(records);
    content
}

impl BackupPreview {
    /// The name of a record on this machine or in the result, for a sentence.
    fn name_of(&self, id: HostId) -> String {
        if let Some(name) = self.local_names.get(&id) {
            return name.clone();
        }
        self.plan
            .result()
            .hosts
            .connection(id)
            .map_or_else(|| id.to_string(), |record| record.name().to_owned())
    }
}

/// A private key path the imported data names that does not exist on this machine.
struct MissingKey {
    id: HostId,
    name: String,
    path: String,
    /// The path is the one another kind of machine used, because the document had none
    /// for this platform.
    foreign: bool,
}

/// What a test reads of an export: the file, the passwords written, whether the
/// encrypted vault travelled with it, the names whose password could not be read,
/// and the names whose password the store does not have.
#[cfg(test)]
pub type ExportReportForTest = (PathBuf, Option<usize>, bool, Vec<String>, Vec<String>);

/// The default name of a backup file: `shellune-backup-YYYYMMDD.json`.
#[must_use]
pub(crate) fn default_backup_name(now_unix: u64) -> String {
    // "2026-11-16 08:00 UTC" -> "20261116".
    let date: String = crate::app::hosts::format_unix(now_unix)
        .chars()
        .take(10)
        .filter(char::is_ascii_digit)
        .collect();
    format!("shellune-backup-{date}.json")
}

/// The cost of sealing the passwords: the storage layer's default, and the cheapest cost
/// Argon2 accepts under the unit tests so they do not spend seconds per export.
fn backup_kdf() -> KdfParams {
    if cfg!(test) {
        KdfParams::new(8, 1, 1).unwrap_or_default()
    } else {
        KdfParams::default()
    }
}

/// A platform name as a person writes it.
fn platform_label(name: &str) -> String {
    match name {
        "windows" => "Windows".to_owned(),
        "macos" => "macOS".to_owned(),
        "linux" => "Linux".to_owned(),
        other => other.to_owned(),
    }
}

/// Why the storage layer refused, in the window's language, with its own words after.
fn backup_error_text(error: &PortableError, strings: &BackupStrings) -> String {
    let detail = |reason: &str| copy::fill(strings.error_detail, &[("reason", reason)]);
    match error {
        PortableError::NotShelluneData { reason } => {
            format!("{} {}", strings.error_not_backup, detail(reason))
        }
        PortableError::UnsupportedVersion { found, supported } => copy::fill(
            strings.error_newer,
            &[
                ("found", &found.to_string()),
                ("supported", &supported.to_string()),
            ],
        ),
        PortableError::Corrupt { reason } => {
            format!("{} {}", strings.error_corrupt, detail(reason))
        }
        PortableError::WrongPassphrase => strings.wrong_passphrase.to_owned(),
        PortableError::EmptyPassphrase => strings.passphrase_required.to_owned(),
        PortableError::Read { reason, .. } => format!("{} {}", strings.error_read, detail(reason)),
        PortableError::Write { reason, .. } => {
            format!("{} {}", strings.error_write, detail(reason))
        }
        PortableError::Local { label, reason } => format!(
            "{} {}",
            strings.error_local,
            detail(&format!("{label}: {reason}"))
        ),
        PortableError::Encrypt { reason } | PortableError::Encode { reason } => {
            format!("{} {}", strings.error_encrypt, detail(reason))
        }
        PortableError::Snapshot { reason, .. } => {
            format!("{} {}", strings.error_snapshot, detail(reason))
        }
        PortableError::Changed => strings.error_changed.to_owned(),
        PortableError::Apply {
            step,
            reason,
            restored,
            snapshot,
        } => {
            let undone = if *restored {
                strings.error_apply_restored.to_owned()
            } else {
                copy::fill(
                    strings.error_apply_not_restored,
                    &[("path", &snapshot.display().to_string())],
                )
            };
            format!(
                "{} {undone} {}",
                strings.error_apply,
                detail(&format!("{step}: {reason}"))
            )
        }
    }
}

/// The start of a fingerprint, enough to tell two apart in a sentence.
fn short_fingerprint(fingerprint: &str) -> String {
    const SHOWN: usize = 20;
    if fingerprint.chars().count() <= SHOWN {
        return fingerprint.to_owned();
    }
    let start: String = fingerprint.chars().take(SHOWN).collect();
    format!("{start}\u{2026}")
}

/// The private key paths of the records the document brings that are not files here.
fn missing_keys(document: &ShelluneData, plan: &ApplyPlan) -> Vec<MissingKey> {
    let foreign: HashSet<HostId> = plan
        .fallbacks()
        .iter()
        .filter(|fallback| fallback.field == PlatformField::IdentityFile)
        .map(|fallback| fallback.id)
        .collect();
    let brought = document.shared().hosts();
    plan.result()
        .hosts
        .hosts()
        .iter()
        .filter(|host| brought.host(host.id()).is_some())
        .filter_map(|host| {
            let path = host.identity_file()?;
            if shellune_storage::resolve_path(path).exists() {
                return None;
            }
            Some(MissingKey {
                id: host.id(),
                name: host.name().to_owned(),
                path: path.to_owned(),
                foreign: foreign.contains(&host.id()),
            })
        })
        .collect()
}

// A checkbox with its label is the shared component.
use crate::app::components::checkbox;

impl ShelluneView {
    /// The backup strings of the language on screen.
    fn backup_strings(&self) -> &'static BackupStrings {
        copy::backup_for(self.workspace.locale())
    }

    /// A masked field for a passphrase.
    fn passphrase_field(
        &self,
        placeholder: &'static str,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        let field = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        // The dialog reads the fields while it draws, so a keystroke has to redraw it:
        // the export button follows the two passphrases as they are typed.
        cx.observe(&field, |_view, _field, cx| cx.notify()).detach();
        field
    }

    // -----------------------------------------------------------------------
    // Export
    // -----------------------------------------------------------------------

    /// Opens the export dialog with the passwords left out.
    pub(super) fn open_backup_export(&mut self, cx: &mut Context<Self>) {
        let strings = self.backup_strings();
        let passphrase = self.passphrase_field(strings.passphrase, cx);
        let confirm = self.passphrase_field(strings.passphrase_confirm, cx);
        let vault_passphrase = self.passphrase_field(strings.export_vault_passphrase, cx);
        let vault_confirm = self.passphrase_field(strings.export_vault_confirm, cx);
        let view = cx.entity().downgrade();
        for field in [confirm.clone(), vault_confirm.clone()] {
            let view = view.clone();
            field.update(cx, |field, _cx| {
                field.on_submit(move |_text, _window, app| {
                    view.update(app, |view, cx| {
                        view.choose_backup_export_path(cx);
                        false
                    })
                    .unwrap_or(false)
                });
            });
        }
        self.backup = Some(BackupDialog::Export(ExportDialog {
            include_secrets: false,
            include_vault: false,
            passphrase,
            confirm,
            vault_passphrase,
            vault_confirm,
            choosing: false,
            running: false,
            failure: None,
            report: None,
        }));
        cx.notify();
    }

    /// Turns the passwords on or off for the next export.
    pub(super) fn toggle_backup_secrets(&mut self, cx: &mut Context<Self>) {
        if let Some(BackupDialog::Export(dialog)) = self.backup.as_mut() {
            dialog.include_secrets = !dialog.include_secrets;
            // A document carries either the version 1 password block or the
            // version 2 vault, never both; the vault is the whole backup, so
            // choosing it puts the old block away (R9).
            if dialog.include_secrets {
                dialog.include_vault = false;
            }
            dialog.failure = None;
            // The first field the dialog now asks for takes the keyboard.
            if dialog.include_secrets {
                self.field_focus_pending = Some(dialog.passphrase.read(cx).focus_handle().clone());
            }
            cx.notify();
        }
    }

    /// Turns the vault on or off for the next export: the whole of it -
    /// keys, passwords, metadata, references and trust - sealed under the
    /// sync passphrase (R9).
    pub(super) fn toggle_backup_vault(&mut self, cx: &mut Context<Self>) {
        let saved = self.saved_sync_passphrase().is_some();
        if let Some(BackupDialog::Export(dialog)) = self.backup.as_mut() {
            dialog.include_vault = !dialog.include_vault;
            if dialog.include_vault {
                dialog.include_secrets = false;
                if !saved {
                    self.field_focus_pending =
                        Some(dialog.vault_passphrase.read(cx).focus_handle().clone());
                }
            }
            dialog.failure = None;
            cx.notify();
        }
    }

    /// Why the export cannot start yet, when it cannot: the passwords need a passphrase,
    /// typed the same way twice; the vault needs a non-empty sync passphrase, which the
    /// saved one already satisfies (R9).
    fn export_refusal(&self, dialog: &ExportDialog, cx: &App) -> Option<&'static str> {
        let strings = self.backup_strings();
        if dialog.include_vault {
            if self.saved_sync_passphrase().is_some() {
                return None;
            }
            let first = dialog.vault_passphrase.read(cx).text();
            if first.trim().is_empty() {
                return Some(strings.export_vault_required);
            }
            if first != dialog.vault_confirm.read(cx).text() {
                return Some(strings.export_vault_mismatch);
            }
            return None;
        }
        if !dialog.include_secrets {
            return None;
        }
        let first = dialog.passphrase.read(cx).text();
        if first.trim().is_empty() {
            return Some(strings.passphrase_required);
        }
        if first != dialog.confirm.read(cx).text() {
            return Some(strings.passphrase_mismatch);
        }
        None
    }

    /// Opens the system's save dialog on the default file name, and exports to the file
    /// it returns. A refused or cancelled choice writes nothing.
    pub(super) fn choose_backup_export_path(&mut self, cx: &mut Context<Self>) {
        let refused = match self.backup.as_ref() {
            Some(BackupDialog::Export(dialog)) => {
                dialog.choosing || dialog.running || self.export_refusal(dialog, cx).is_some()
            }
            _ => true,
        };
        if refused {
            return;
        }
        // A backup that carries the whole vault is every secret leaving the
        // machine at once, so it goes through the same always-verify gate a
        // private-key export does - whatever the policy, and before the file
        // chooser opens, so a cancelled or refused verification leaves no
        // file behind (R3, R9, P1-1). The grant is one passage: the resumed
        // chooser does not ask for the pass it just got.
        let carries_vault = self.backup.as_ref().is_some_and(
            |dialog| matches!(dialog, BackupDialog::Export(dialog) if dialog.include_vault),
        );
        if carries_vault
            && !self.security.operation_granted
            && self.security_begin_always(vec![SecurityPending::BackupVaultExport], cx)
        {
            return;
        }
        self.security.operation_granted = false;
        // A version 1 file seals host passwords the vault holds: they are read
        // from the window's open vault only, so a locked vault asks for its one
        // unlock first, and one still opening is waited for (R4).
        let seals_passwords = self.backup.as_ref().is_some_and(|dialog| {
            matches!(dialog, BackupDialog::Export(dialog)
                if dialog.include_secrets && !dialog.include_vault)
        });
        if seals_passwords && self.security_wait_for_vault(SecurityPending::BackupSecretsExport, cx)
        {
            return;
        }
        if seals_passwords && self.vault_open_for(SecurityPending::BackupSecretsExport, cx) {
            return;
        }
        if let Some(BackupDialog::Export(dialog)) = self.backup.as_mut() {
            dialog.choosing = true;
        }
        let directory = shellune_storage::home_directory().unwrap_or_else(|| PathBuf::from("."));
        let name = default_backup_name(crate::app::remote::now_unix());
        let answer = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let chosen = match answer.await {
                Ok(Ok(Some(path))) => Some(path),
                _ => None,
            };
            let _ = this.update(cx, |view, cx| {
                if let Some(BackupDialog::Export(dialog)) = view.backup.as_mut() {
                    dialog.choosing = false;
                }
                match chosen {
                    Some(path) => view.export_backup_to(path, cx),
                    None => cx.notify(),
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Writes the backup to `path`.
    ///
    /// A backup already at that path is the base: the sections other kinds of machine
    /// wrote into it are carried over, so exporting over a file made on another platform
    /// keeps that platform's shortcuts and key paths.
    pub(super) fn export_backup_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(BackupDialog::Export(dialog)) = self.backup.as_ref() else {
            return;
        };
        if dialog.running || self.export_refusal(dialog, cx).is_some() {
            return;
        }
        let passphrase = dialog
            .include_secrets
            .then(|| dialog.passphrase.read(cx).text().to_owned());
        // The vault is sealed under the sync passphrase: the saved one when
        // there is one, otherwise the one being set here, which becomes the
        // saved one - a user sets this secret once, not per device (R9).
        let vault_passphrase = if dialog.include_vault {
            match self.saved_sync_passphrase() {
                Some(saved) => Some((saved, false)),
                None => Some((
                    SecretString::new(dialog.vault_passphrase.read(cx).text().to_owned()),
                    true,
                )),
            }
        } else {
            None
        };
        let root = self.store.root().to_path_buf();
        let secrets = self.secrets.clone();
        // The passwords a version 1 file seals come from the vault alone (R4):
        // the copy this window holds open. A vault that cannot be read now
        // (locked, or one that could not be opened) is not read behind the
        // lock's back; the export says why instead. Nothing is asked per host.
        let unlocked = self.security.content().cloned();
        let unreadable = self.vault_unreadable_reason();
        let app_version = self.version().to_owned();
        if let Some(BackupDialog::Export(dialog)) = self.backup.as_mut() {
            dialog.running = true;
            dialog.failure = None;
        }
        // Sealing the passwords is an Argon2 derivation that is meant to be slow, and
        // the data files are read and written: none of it happens on the UI thread.
        let target = path.clone();
        let with_secrets = passphrase.is_some();
        let with_vault = vault_passphrase.is_some();
        let vault_passphrase_in = vault_passphrase.clone();
        let work = cx.background_executor().spawn(async move {
            let local = LocalData::at(&root).load()?;
            let mut options = ExportOptions::new(DataPlatform::current());
            options.kdf = backup_kdf();
            options.app_version = app_version;
            if let Some(passphrase) = passphrase {
                options = options.with_passphrase(SecretString::new(passphrase));
            }
            if let Some((passphrase, _)) = &vault_passphrase_in {
                // The vault this machine holds, opened with its own local
                // unseal secret, sealed into the file under the passphrase
                // alone: a copy no WebDAV password is needed to read (R9).
                let content = VaultStore::at(&root)
                    .open(&UnsealSource::LocalMachine, secrets.as_ref())
                    .map_err(|error| PortableError::Local {
                        label: "vault",
                        reason: error.to_string(),
                    })?;
                let cipher =
                    shellune_storage::VaultCipher::export_copy(&content, passphrase, backup_kdf())
                        .map_err(|error| PortableError::Encrypt {
                            reason: error.to_string(),
                        })?;
                options = options.with_vault(cipher);
            }
            let base = ShelluneData::read(&target).ok();
            let sealed = if with_secrets && options.vault().is_none() {
                match (unlocked, unreadable) {
                    (Some(content), _) => Some(content),
                    (None, None) => None,
                    (None, Some(reason)) => {
                        return Err(PortableError::Local {
                            label: "vault",
                            reason,
                        });
                    }
                }
            } else {
                None
            };
            let outcome =
                ShelluneData::export_from_vault(&local, &options, sealed.as_ref(), base.as_ref())?;
            outcome.document.write_to(&target)?;
            Ok::<_, PortableError>((local, outcome))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_backup_export(
                    path,
                    with_secrets,
                    with_vault,
                    vault_passphrase,
                    result,
                    cx,
                );
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows what the export on the background executor did.
    #[allow(clippy::too_many_arguments)]
    fn finish_backup_export(
        &mut self,
        path: PathBuf,
        with_secrets: bool,
        with_vault: bool,
        vault_passphrase: Option<(SecretString, bool)>,
        result: Result<(LocalState, ExportOutcome), PortableError>,
        cx: &mut Context<Self>,
    ) {
        let strings = self.backup_strings();
        let Some(BackupDialog::Export(dialog)) = self.backup.as_mut() else {
            return;
        };
        dialog.running = false;
        match result {
            Ok((local, outcome)) => {
                let name = |id: &HostId| {
                    local
                        .hosts
                        .connection(*id)
                        .map_or_else(|| id.to_string(), |record| record.name().to_owned())
                };
                let unreadable = outcome.secrets_unreadable.iter().map(name).collect();
                let missing = outcome.secrets_missing.iter().map(name).collect();
                dialog.failure = None;
                dialog.report = Some(ExportReport {
                    path: path.clone(),
                    secrets: with_secrets.then_some(outcome.secrets_included.len()),
                    vault: with_vault,
                    unreadable,
                    missing,
                });
                // The passphrase has done its work; it is not kept on screen.
                let fields = [
                    dialog.passphrase.clone(),
                    dialog.confirm.clone(),
                    dialog.vault_passphrase.clone(),
                    dialog.vault_confirm.clone(),
                ];
                // The one that was set for the first time is kept the only
                // place a secret belongs: the system credential store (R9).
                let saved_note = match vault_passphrase {
                    Some((passphrase, true)) => self.save_sync_passphrase(&passphrase).err(),
                    _ => None,
                };
                for field in fields {
                    field.update(cx, |field, cx| field.set_text(String::new(), cx));
                }
                match saved_note {
                    Some(reason) => {
                        self.toast(Tone::Warning, strings.export_failed, Some(reason), cx);
                    }
                    None => {
                        self.toast(
                            Tone::Positive,
                            strings.export_done,
                            Some(super::home_relative(&path.display().to_string())),
                            cx,
                        );
                    }
                }
            }
            Err(error) => {
                dialog.report = None;
                dialog.failure = Some(format!(
                    "{}. {}",
                    strings.export_failed,
                    backup_error_text(&error, strings)
                ));
            }
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Import
    // -----------------------------------------------------------------------

    /// Opens the system's file chooser and reads the backup it returns.
    pub(super) fn choose_backup_import(&mut self, cx: &mut Context<Self>) {
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
            let _ = this.update(cx, |view, cx| view.read_backup(path, cx));
        })
        .detach();
    }

    /// Reads a backup file and opens the import dialog on it: refused with its reason,
    /// asking for the passphrase (the sealed passwords of a version 1 file, or the
    /// encrypted vault of a version 2 one), or showing the preview.
    pub(super) fn read_backup(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let strings = self.backup_strings();
        let stage = match ShelluneData::read(&path) {
            Err(error) => ImportStage::Refused(backup_error_text(&error, strings)),
            Ok(document) if document.has_secrets() => {
                let field = self.passphrase_field(strings.passphrase, cx);
                let view = cx.entity().downgrade();
                field.update(cx, |field, _cx| {
                    field.on_submit(move |_text, _window, app| {
                        view.update(app, |view, cx| {
                            view.unlock_backup(cx);
                            false
                        })
                        .unwrap_or(false)
                    });
                });
                // The passphrase is the one thing this step asks for.
                self.field_focus_pending = Some(field.read(cx).focus_handle().clone());
                ImportStage::Locked {
                    document: Box::new(document),
                    field,
                    problem: None,
                    running: false,
                }
            }
            Ok(document) if document.has_vault() => {
                // A version 2 backup is sealed under the sync passphrase and
                // nothing else; the vault cannot even be counted before it
                // opens (R9).
                let field = self.passphrase_field(strings.export_vault_passphrase, cx);
                let view = cx.entity().downgrade();
                field.update(cx, |field, _cx| {
                    field.on_submit(move |_text, _window, app| {
                        view.update(app, |view, cx| {
                            view.unlock_backup_vault(cx);
                            false
                        })
                        .unwrap_or(false)
                    });
                });
                self.field_focus_pending = Some(field.read(cx).focus_handle().clone());
                ImportStage::VaultLocked {
                    document: Box::new(document),
                    field,
                    problem: None,
                    running: false,
                }
            }
            Ok(document) => self.plan_backup(document, Secrets::NotInFile, None, ApplyMode::Merge),
        };
        self.backup = Some(BackupDialog::Import(ImportDialog { path, stage }));
        cx.notify();
    }

    /// Plans applying a document to the data as it is on disk now.
    fn plan_backup(
        &self,
        document: ShelluneData,
        secrets: Secrets,
        vault: Option<BackupVault>,
        mode: ApplyMode,
    ) -> ImportStage {
        let strings = self.backup_strings();
        let local = match LocalData::at(self.store.root()).load() {
            Ok(local) => local,
            Err(error) => return ImportStage::Refused(backup_error_text(&error, strings)),
        };
        // The remembered passwords a version 2 backup's vault holds mark their
        // records as remembering, as a version 1 block does (ADR-6): without
        // that, a restored password would belong to a record that never
        // reads it, and the next save would drop it.
        let from_vault = vault
            .as_ref()
            .map(|vault| SecretBundle::from_vault(&vault.content));
        let bundle = match &secrets {
            Secrets::Opened(bundle) => Some(bundle),
            Secrets::NotInFile | Secrets::Skipped => from_vault.as_ref(),
        };
        match document.plan(&local, DataPlatform::current(), mode, bundle) {
            Ok(plan) => ImportStage::Preview(Box::new(BackupPreview {
                missing_keys: missing_keys(&document, &plan),
                local_names: local
                    .hosts
                    .records()
                    .into_iter()
                    .map(|record| (record.id(), record.name().to_owned()))
                    .collect(),
                document,
                secrets,
                mode,
                plan,
                vault,
                confirm_replace: false,
                problem: None,
            })),
            Err(error) => ImportStage::Refused(backup_error_text(&error, strings)),
        }
    }

    /// Tries the typed passphrase on the document's encrypted vault, on the
    /// background executor: the derivation is slow on purpose (R9).
    pub(super) fn unlock_backup_vault(&mut self, cx: &mut Context<Self>) {
        let strings = self.backup_strings();
        let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() else {
            return;
        };
        let ImportStage::VaultLocked {
            document,
            field,
            problem,
            running,
        } = &mut dialog.stage
        else {
            return;
        };
        if *running {
            return;
        }
        let typed = field.read(cx).text().to_owned();
        if typed.trim().is_empty() {
            *problem = Some(strings.import_passphrase_required.to_owned());
            cx.notify();
            return;
        }
        *running = true;
        *problem = None;
        let document = (**document).clone();
        let secret = SecretString::new(typed);
        let open_with = secret.clone();
        let work = cx.background_executor().spawn(async move {
            let opened = document.vault().and_then(|cipher| {
                cipher
                    .open(&KeySource::Passphrase {
                        purpose: PassphrasePurpose::SyncPassphrase,
                        value: open_with,
                    })
                    .ok()
            });
            (document, opened, secret)
        });
        cx.spawn(async move |this, cx| {
            let (document, opened, secret) = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_backup_vault_unlock(document, opened, secret, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// Goes on to the preview with the opened vault, or back to the question.
    /// A wrong passphrase is a wrong secret, not a damaged file: the file is
    /// refused nothing, and nothing has been written (R9).
    fn finish_backup_vault_unlock(
        &mut self,
        document: ShelluneData,
        opened: Option<VaultContent>,
        passphrase: SecretString,
        cx: &mut Context<Self>,
    ) {
        let strings = self.backup_strings();
        let still_asking = matches!(
            self.backup.as_ref(),
            Some(BackupDialog::Import(ImportDialog {
                stage: ImportStage::VaultLocked { .. },
                ..
            }))
        );
        if !still_asking {
            return;
        }
        match opened {
            Some(content) => {
                let vault = BackupVault {
                    content,
                    passphrase,
                };
                let stage =
                    self.plan_backup(document, Secrets::NotInFile, Some(vault), ApplyMode::Merge);
                self.put_backup_stage(stage);
            }
            None => {
                let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() else {
                    return;
                };
                let ImportStage::VaultLocked {
                    field,
                    problem,
                    running,
                    ..
                } = &mut dialog.stage
                else {
                    return;
                };
                *running = false;
                *problem = Some(strings.wrong_passphrase.to_owned());
                let field = field.clone();
                field.update(cx, |field, cx| field.set_text(String::new(), cx));
                self.field_focus_pending = Some(field.read(cx).focus_handle().clone());
            }
        }
        cx.notify();
    }

    /// Opens the sealed passwords with the passphrase typed, or says it is wrong and
    /// stays on the question. The passphrase is tried on the background executor: the
    /// key derivation is slow on purpose.
    pub(super) fn unlock_backup(&mut self, cx: &mut Context<Self>) {
        let strings = self.backup_strings();
        let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() else {
            return;
        };
        let ImportStage::Locked {
            document,
            field,
            problem,
            running,
        } = &mut dialog.stage
        else {
            return;
        };
        if *running {
            return;
        }
        let typed = field.read(cx).text().to_owned();
        if typed.trim().is_empty() {
            *problem = Some(strings.import_passphrase_required.to_owned());
            cx.notify();
            return;
        }
        *running = true;
        *problem = None;
        let document = (**document).clone();
        let work = cx.background_executor().spawn(async move {
            let opened = document.open_secrets(&SecretString::new(typed));
            (document, opened)
        });
        cx.spawn(async move |this, cx| {
            let (document, opened) = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_backup_unlock(document, opened, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// Goes on to the preview with the opened passwords, or back to the question.
    fn finish_backup_unlock(
        &mut self,
        document: ShelluneData,
        opened: Result<SecretBundle, PortableError>,
        cx: &mut Context<Self>,
    ) {
        let strings = self.backup_strings();
        let still_asking = matches!(
            self.backup.as_ref(),
            Some(BackupDialog::Import(ImportDialog {
                stage: ImportStage::Locked { .. },
                ..
            }))
        );
        if !still_asking {
            return;
        }
        match opened {
            Ok(bundle) => {
                let stage =
                    self.plan_backup(document, Secrets::Opened(bundle), None, ApplyMode::Merge);
                self.put_backup_stage(stage);
            }
            Err(error) => {
                let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() else {
                    return;
                };
                let ImportStage::Locked {
                    field,
                    problem,
                    running,
                    ..
                } = &mut dialog.stage
                else {
                    return;
                };
                *running = false;
                *problem = Some(backup_error_text(&error, strings));
                let field = field.clone();
                field.update(cx, |field, cx| field.set_text(String::new(), cx));
                self.field_focus_pending = Some(field.read(cx).focus_handle().clone());
            }
        }
        cx.notify();
    }

    /// Goes on to the preview without opening the passwords.
    pub(super) fn skip_backup_secrets(&mut self, cx: &mut Context<Self>) {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_ref() else {
            return;
        };
        let ImportStage::Locked { document, .. } = &dialog.stage else {
            return;
        };
        let document = (**document).clone();
        let stage = self.plan_backup(document, Secrets::Skipped, None, ApplyMode::Merge);
        if let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() {
            dialog.stage = stage;
        }
        cx.notify();
    }

    /// Takes the preview out of the dialog, for a step that replaces it.
    fn take_backup_preview(&mut self) -> Option<Box<BackupPreview>> {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() else {
            return None;
        };
        if !matches!(dialog.stage, ImportStage::Preview(_)) {
            return None;
        }
        match std::mem::replace(&mut dialog.stage, ImportStage::Refused(String::new())) {
            ImportStage::Preview(preview) => Some(preview),
            other => {
                dialog.stage = other;
                None
            }
        }
    }

    fn put_backup_stage(&mut self, stage: ImportStage) {
        if let Some(BackupDialog::Import(dialog)) = self.backup.as_mut() {
            dialog.stage = stage;
        }
    }

    /// Chooses merge or replace; the preview is planned again for that mode.
    pub(super) fn set_backup_mode(&mut self, mode: ApplyMode, cx: &mut Context<Self>) {
        let Some(preview) = self.take_backup_preview() else {
            return;
        };
        if preview.mode == mode {
            self.put_backup_stage(ImportStage::Preview(preview));
            return;
        }
        let BackupPreview {
            document,
            secrets,
            vault,
            ..
        } = *preview;
        let stage = self.plan_backup(document, secrets, vault, mode);
        self.put_backup_stage(stage);
        cx.notify();
    }

    /// Applies the preview. A replace asks once more first; a change on disk since the
    /// preview plans it again instead of writing over that change. The files are
    /// written on the background executor.
    pub(super) fn apply_backup(&mut self, cx: &mut Context<Self>) {
        let Some(mut preview) = self.take_backup_preview() else {
            return;
        };
        if preview.mode == ApplyMode::Replace && !preview.confirm_replace {
            preview.confirm_replace = true;
            self.put_backup_stage(ImportStage::Preview(preview));
            cx.notify();
            return;
        }
        self.put_backup_stage(ImportStage::Applying);
        cx.notify();
        // A sync job may be writing the same files: the import waits for the slot
        // the jobs share, so two writers never apply at once.
        self.when_sync_slot_free(
            Box::new(move |view, cx| view.start_backup_apply(preview, cx)),
            cx,
        );
    }

    /// Applies the preview on the background executor, holding the slot the sync jobs
    /// use and holding the window's own saves back until the files are read again.
    fn start_backup_apply(&mut self, preview: Box<BackupPreview>, cx: &mut Context<Self>) {
        let still_open = matches!(
            self.backup.as_ref(),
            Some(BackupDialog::Import(ImportDialog {
                stage: ImportStage::Applying,
                ..
            }))
        );
        if !still_open {
            // Closed while it waited: nothing was written, and nothing is.
            self.start_next_in_sync_slot(cx);
            return;
        }
        // An apply that writes the vault file waits for the vault job running
        // now, holding the slot meanwhile, and holds every other vault job
        // back until it ends: two writers each saving what they opened would
        // drop the other's host passwords (R4).
        let writes_vault = preview.vault.is_some() || preview.plan.has_secrets();
        if writes_vault && self.security_job_running() {
            self.set_backup_in_sync_slot(true);
            self.security_when_no_job(
                Box::new(move |view, cx| {
                    view.set_backup_in_sync_slot(false);
                    view.start_backup_apply(preview, cx);
                }),
                cx,
            );
            return;
        }
        if writes_vault {
            // The old entries of hosts the import keeps are left alone: an
            // import never touches `host-<uuid>` entries (R4). A live vault
            // record is read before any of them, and the entry goes at the
            // host's next deletion or untick.
            self.security_set_import_writing(true, cx);
        }
        self.set_backup_in_sync_slot(true);
        self.hold_data_writes();
        let data = LocalData::at(self.store.root());
        let secrets = self.secrets.clone();
        let root = self.store.root().to_path_buf();
        let device = self.security.device().to_owned();
        // The vault this machine holds as the window has it open: a restore
        // keeps the host passwords the backup says nothing about.
        let local_vault = self.security.content().cloned();
        let reason = match preview.mode {
            ApplyMode::Merge => "backup-merge",
            ApplyMode::Replace => "backup-replace",
        };
        let work = cx.background_executor().spawn(async move {
            // The vault is checked against the book the apply would end with
            // before anything is written: a backup whose bindings cannot hold
            // fails whole, and this machine keeps what it had (R9).
            if let Some(vault) = &preview.vault {
                let records = preview.plan.result().hosts.records();
                let mut content = vault.content.clone();
                content.set_references(&HostReference::snapshot_records(&records));
                if let Err(error) = content.validate_for_records(&records) {
                    return (
                        preview,
                        Err(PortableError::Corrupt {
                            reason: format!("the backup's vault cannot be applied: {error}"),
                        }),
                        BackupVaultOutcome::NotInFile,
                    );
                }
            }
            if preview.vault.is_none() && preview.plan.has_secrets() {
                // The passwords a version 1 backup seals land in this machine's
                // vault, saved together with the files: a vault that cannot be
                // saved puts the files back, and nothing is asked of the system
                // store per host (R4).
                let (outcome, vault_outcome) =
                    apply_into_vault(&preview, &data, &root, &device, reason, secrets.as_ref());
                return (preview, outcome, vault_outcome);
            }
            let (outcome, vault_outcome) = match preview.vault.as_ref() {
                // The document applies; the vault follows as one piece,
                // sealed for this machine and for the passphrase that opened
                // it (R9), keeping this machine's host passwords the backup
                // has no record of (R4).
                Some(vault) => restore_into_vault(
                    &preview,
                    vault,
                    &data,
                    &root,
                    &device,
                    reason,
                    secrets.as_ref(),
                    local_vault,
                ),
                None => (
                    preview.plan.apply(&data, reason),
                    BackupVaultOutcome::NotInFile,
                ),
            };
            (preview, outcome, vault_outcome)
        });
        cx.spawn(async move |this, cx| {
            let (preview, outcome, vault_outcome) = work.await;
            let _ = this.update(cx, |view, cx| {
                view.finish_backup_apply(preview, outcome, vault_outcome, cx)
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows what applying did: the result, a fresh preview of data that changed in
    /// between, or the preview again with the reason.
    fn finish_backup_apply(
        &mut self,
        preview: Box<BackupPreview>,
        outcome: Result<ApplyOutcome, PortableError>,
        vault_outcome: BackupVaultOutcome,
        cx: &mut Context<Self>,
    ) {
        self.set_backup_in_sync_slot(false);
        // What the window saved meanwhile is merged onto the imported files first.
        let held = self.release_data_writes();
        let changed = outcome.is_ok() && !preview.plan.is_empty();
        self.show_backup_apply(preview, outcome, vault_outcome, cx);
        // The vault file is no longer being written: the vault jobs that
        // waited (the deletions this import queued among them) go now.
        self.security_set_import_writing(false, cx);
        self.settle_held_writes(held, false, cx);
        // The imported data is a change like any other: with automatic sync on it is
        // uploaded after the debounce (R8).
        if changed {
            self.note_data_changed(cx);
        }
        self.start_next_in_sync_slot(cx);
        cx.notify();
    }

    /// Shows what applying did and reads the written files back.
    fn show_backup_apply(
        &mut self,
        mut preview: Box<BackupPreview>,
        outcome: Result<ApplyOutcome, PortableError>,
        vault_outcome: BackupVaultOutcome,
        cx: &mut Context<Self>,
    ) {
        let strings = self.backup_strings();
        let still_open = matches!(
            self.backup.as_ref(),
            Some(BackupDialog::Import(ImportDialog {
                stage: ImportStage::Applying,
                ..
            }))
        );
        if !still_open {
            // The dialog was closed meanwhile; what was written is on disk, so the
            // lists follow it all the same.
            if outcome.is_ok() {
                self.adopt_imported_data(preview.plan.preferences_changed(), cx);
                let forgotten = preview.plan.secrets_to_forget().to_vec();
                if !forgotten.is_empty() {
                    // Records the import stopped remembering take their passwords
                    // out of the vault, and any old entry with them (R1).
                    self.host_secrets_forget(&forgotten, cx);
                }
                if matches!(
                    vault_outcome,
                    BackupVaultOutcome::Restored | BackupVaultOutcome::PasswordsSaved
                ) {
                    self.vault_reread(cx);
                }
            }
            return;
        }
        match outcome {
            Ok(outcome) => {
                let changed = !preview.plan.is_empty();
                let not_forgotten = outcome
                    .secrets_not_forgotten
                    .iter()
                    .map(|id| preview.name_of(*id))
                    .collect();
                self.adopt_imported_data(preview.plan.preferences_changed(), cx);
                let forgotten = preview.plan.secrets_to_forget().to_vec();
                if !forgotten.is_empty() {
                    // Records the import stopped remembering take their passwords
                    // out of the vault, and any old entry with them (R1).
                    self.host_secrets_forget(&forgotten, cx);
                }
                let restored = matches!(vault_outcome, BackupVaultOutcome::Restored);
                if restored || matches!(vault_outcome, BackupVaultOutcome::PasswordsSaved) {
                    // The window's copy of the vault is the file that was just
                    // written; the secrets, references and trust it holds are
                    // what the connections read from now on (R9).
                    self.vault_reread(cx);
                }
                self.put_backup_stage(ImportStage::Applied {
                    snapshot: outcome.snapshot,
                    changed,
                    saved: outcome.secrets_saved,
                    vault: restored,
                    not_forgotten,
                });
                match &vault_outcome {
                    BackupVaultOutcome::Restored => {
                        self.toast(
                            Tone::Positive,
                            strings.import_done,
                            Some(strings.import_done_vault.to_owned()),
                            cx,
                        );
                    }
                    BackupVaultOutcome::Failed(reason) => {
                        self.toast(Tone::Warning, strings.import_done, Some(reason.clone()), cx);
                    }
                    BackupVaultOutcome::NotInFile | BackupVaultOutcome::PasswordsSaved => {
                        self.toast(Tone::Positive, strings.import_done, None, cx);
                    }
                }
            }
            Err(PortableError::Changed) => {
                let BackupPreview {
                    document,
                    secrets,
                    vault,
                    mode,
                    ..
                } = *preview;
                let mut stage = self.plan_backup(document, secrets, vault, mode);
                if let ImportStage::Preview(preview) = &mut stage {
                    preview.problem = Some(strings.error_changed.to_owned());
                }
                self.put_backup_stage(stage);
            }
            Err(error) => {
                preview.problem = Some(backup_error_text(&error, strings));
                preview.confirm_replace = false;
                self.put_backup_stage(ImportStage::Preview(preview));
            }
        }
        cx.notify();
    }

    /// Reads every store again after an import, so the lists, the trust decisions and
    /// the preferences on screen are the ones now on disk. A pull from the cloud is
    /// adopted the same way (`view/sync.rs`).
    pub(super) fn adopt_imported_data(&mut self, preferences: bool, cx: &mut Context<Self>) {
        self.reload_hosts(cx);
        self.restore_groups();
        self.restore_trust();
        self.reload_forwards();
        self.snippets = SnippetLibrary::load(SnippetStore::at(self.store.root()));
        if preferences {
            let (saved, problem) =
                shellune_storage::PreferencesStore::at(self.store.root()).load_reported();
            self.apply_preferences(saved, problem, cx);
            self.relabel_fields(cx);
            self.repaint_terminals(cx);
            let size = self.workspace.local_font_size();
            self.set_terminal_font_size(size, cx);
        }
        cx.notify();
    }

    /// Closes whichever backup dialog is open; nothing is written.
    pub(super) fn close_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup.take().is_some() {
            self.restore_focus_pending = true;
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// The open backup dialog, drawn over the window.
    pub(super) fn backup_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        match self.backup.as_ref()? {
            BackupDialog::Export(dialog) => Some(self.export_dialog(dialog, window, cx)),
            BackupDialog::Import(dialog) => Some(self.import_dialog(dialog, window, cx)),
        }
    }

    /// A caption line.
    fn backup_caption(&self, text: impl Into<SharedString>, selector: &'static str) -> Div {
        let metrics = self.metrics();
        div()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .debug_selector(move || selector.to_owned())
            .child(text.into())
    }

    /// A coloured notice with its own selector.
    fn backup_banner(
        &self,
        tone: Tone,
        text: impl Into<SharedString>,
        selector: &'static str,
    ) -> Div {
        components::banner(tone, self.metrics())
            .debug_selector(move || selector.to_owned())
            .whitespace_normal()
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .child(text.into())
    }

    /// A field under its label, stacked so both fit the smallest window at 200%.
    fn backup_field(
        &self,
        label: &'static str,
        selector: &'static str,
        field: &Entity<TextField>,
    ) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(label),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .debug_selector(move || selector.to_owned())
                    .child(field.clone()),
            )
    }

    /// The footer row of a backup dialog.
    fn backup_footer(&self) -> Div {
        let metrics = self.metrics();
        components::modal_footer(metrics)
    }

    /// The main button of a backup dialog, drawn quiet and inert while it cannot act.
    fn backup_primary(
        &self,
        id: &'static str,
        label: &'static str,
        variant: Variant,
        enabled: bool,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let button = components::compact_button(label, variant, self.metrics())
            .id(id)
            .debug_selector(move || id.to_owned());
        if enabled {
            button.on_click(cx.listener(move |this, _event, _window, cx| click(this, cx)))
        } else {
            button.opacity(0.5)
        }
    }

    /// Wraps a dialog body so Escape closes the dialog instead of emptying a field.
    fn backup_body(&self, selector: &'static str, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .debug_selector(move || selector.to_owned())
            .capture_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_backup(cx);
                    }
                }),
            )
    }

    fn export_dialog(&self, dialog: &ExportDialog, window: &Window, cx: &mut Context<Self>) -> Div {
        let strings = self.backup_strings();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Transfer),
            strings.export_title.to_owned(),
            Some(strings.export_intro.to_owned()),
            "backup.export.close",
            cx,
            |this, cx| this.close_backup(cx),
        );
        let refusal = self.export_refusal(dialog, cx);
        let mut body = self
            .backup_body("backup.export.body", cx)
            .child(
                checkbox(
                    "backup.export.vault",
                    strings.export_include_vault,
                    dialog.include_vault,
                    self.metrics(),
                )
                .on_click(cx.listener(|this, _event, _window, cx| this.toggle_backup_vault(cx))),
            )
            .child(
                checkbox(
                    "backup.export.secrets",
                    strings.export_include_secrets,
                    dialog.include_secrets,
                    self.metrics(),
                )
                .on_click(cx.listener(|this, _event, _window, cx| this.toggle_backup_secrets(cx))),
            );
        if dialog.include_vault {
            body = body
                .child(self.backup_caption(strings.export_vault_hint, "backup.export.vault.hint"));
            if self.saved_sync_passphrase().is_some() {
                body = body.child(
                    self.backup_caption(strings.export_vault_saved, "backup.export.vault.saved"),
                );
            } else {
                body = body
                    .child(self.backup_field(
                        strings.export_vault_passphrase,
                        "backup.export.vault.passphrase",
                        &dialog.vault_passphrase,
                    ))
                    .child(self.backup_field(
                        strings.export_vault_confirm,
                        "backup.export.vault.confirm",
                        &dialog.vault_confirm,
                    ));
            }
        }
        if dialog.include_secrets {
            body = body
                .child(
                    self.backup_caption(strings.export_secrets_hint, "backup.export.secrets.hint"),
                )
                .child(self.backup_field(
                    strings.passphrase,
                    "backup.export.passphrase",
                    &dialog.passphrase,
                ))
                .child(self.backup_field(
                    strings.passphrase_confirm,
                    "backup.export.confirm",
                    &dialog.confirm,
                ));
            if let Some(refusal) = refusal {
                body =
                    body.child(self.backup_banner(Tone::Warning, refusal, "backup.export.refusal"));
            }
        } else {
            body =
                body.child(self.backup_caption(strings.export_no_secrets, "backup.export.plain"));
        }
        if let Some(failure) = &dialog.failure {
            body = body.child(self.backup_banner(
                Tone::Danger,
                failure.clone(),
                "backup.export.failure",
            ));
        }
        if let Some(report) = &dialog.report {
            body = body.child(self.export_report(report));
        }
        let done = dialog.report.is_some();
        let footer = self
            .backup_footer()
            .child(self.text_button(
                SharedString::from("backup.export.cancel"),
                if done { strings.close } else { strings.cancel },
                Variant::Ghost,
                cx,
                |this, cx| this.close_backup(cx),
            ))
            .child(self.backup_primary(
                "backup.export.save",
                if dialog.running {
                    strings.export_running
                } else {
                    strings.export_choose
                },
                if done {
                    Variant::Secondary
                } else {
                    Variant::Primary
                },
                refusal.is_none() && !dialog.choosing && !dialog.running,
                cx,
                |this, cx| this.choose_backup_export_path(cx),
            ));
        self.modal(
            window,
            "backup.export.dialog",
            520.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// What an export wrote, and the passwords it could not take along.
    fn export_report(&self, report: &ExportReport) -> Div {
        let strings = self.backup_strings();
        let metrics = self.metrics();
        let path = super::home_relative(&report.path.display().to_string());
        let secrets = match report.secrets {
            Some(count) => copy::fill(
                strings.export_done_secrets,
                &[("count", &count.to_string())],
            ),
            None => strings.export_done_no_secrets.to_owned(),
        };
        let mut card = components::banner(Tone::Positive, metrics)
            .debug_selector(|| "backup.export.report".to_owned())
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .whitespace_normal()
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(strings.export_done),
            )
            .when(report.vault, |card| {
                card.child(
                    div()
                        .debug_selector(|| "backup.export.report.vault".to_owned())
                        .child(strings.export_done_vault),
                )
            })
            .child(
                div()
                    .debug_selector(|| "backup.export.report.path".to_owned())
                    .child(SharedString::from(copy::fill(
                        strings.export_done_path,
                        &[("path", &path)],
                    ))),
            )
            .child(SharedString::from(secrets));
        for (names, intro, selector) in [
            (
                &report.unreadable,
                strings.export_unreadable,
                "backup.export.unreadable",
            ),
            (
                &report.missing,
                strings.export_missing,
                "backup.export.missing",
            ),
        ] {
            if names.is_empty() {
                continue;
            }
            card = card.child(
                div()
                    .debug_selector(move || selector.to_owned())
                    .text_color(Tone::Warning.text())
                    .child(SharedString::from(format!("{intro} {}", names.join(", ")))),
            );
        }
        card
    }

    fn import_dialog(&self, dialog: &ImportDialog, window: &Window, cx: &mut Context<Self>) -> Div {
        let strings = self.backup_strings();
        let file = dialog.path.file_name().map_or_else(
            || dialog.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let (header, subtitle) = self.modal_header(
            Some(Icon::Import),
            strings.import_title.to_owned(),
            Some(file),
            "backup.import.close",
            cx,
            |this, cx| this.close_backup(cx),
        );
        let body = self.backup_body("backup.import.body", cx);
        let footer = self.backup_footer();
        let (body, footer) = match &dialog.stage {
            ImportStage::Refused(reason) => (
                body.child(self.backup_banner(
                    Tone::Danger,
                    strings.import_refused,
                    "backup.import.refused",
                ))
                .child(self.backup_caption(reason.clone(), "backup.import.reason")),
                footer.child(self.backup_primary(
                    "backup.import.done",
                    strings.close,
                    Variant::Primary,
                    true,
                    cx,
                    |this, cx| this.close_backup(cx),
                )),
            ),
            ImportStage::Locked {
                field,
                problem,
                running,
                ..
            } => {
                let running = *running;
                let mut body = body
                    .child(self.backup_caption(strings.import_locked, "backup.import.locked"))
                    .child(self.backup_field(
                        strings.passphrase,
                        "backup.import.passphrase",
                        field,
                    ));
                if let Some(problem) = problem {
                    body = body.child(self.backup_banner(
                        Tone::Danger,
                        problem.clone(),
                        "backup.import.problem",
                    ));
                }
                (
                    body,
                    footer
                        .child(self.text_button(
                            SharedString::from("backup.import.cancel"),
                            strings.cancel,
                            Variant::Ghost,
                            cx,
                            |this, cx| this.close_backup(cx),
                        ))
                        .child(self.text_button(
                            SharedString::from("backup.import.skip"),
                            strings.import_skip_secrets,
                            Variant::Secondary,
                            cx,
                            |this, cx| this.skip_backup_secrets(cx),
                        ))
                        .child(self.backup_primary(
                            "backup.import.unlock",
                            if running {
                                strings.import_running
                            } else {
                                strings.import_unlock
                            },
                            Variant::Primary,
                            !running,
                            cx,
                            |this, cx| this.unlock_backup(cx),
                        )),
                )
            }
            ImportStage::VaultLocked {
                field,
                problem,
                running,
                ..
            } => {
                let running = *running;
                let mut body = body
                    .child(
                        self.backup_caption(
                            strings.import_vault_locked,
                            "backup.import.vault.locked",
                        ),
                    )
                    .child(self.backup_field(
                        strings.export_vault_passphrase,
                        "backup.import.vault.passphrase",
                        field,
                    ));
                if let Some(problem) = problem {
                    body = body.child(self.backup_banner(
                        Tone::Danger,
                        problem.clone(),
                        "backup.import.problem",
                    ));
                }
                (
                    body,
                    footer
                        .child(self.text_button(
                            SharedString::from("backup.import.cancel"),
                            strings.cancel,
                            Variant::Ghost,
                            cx,
                            |this, cx| this.close_backup(cx),
                        ))
                        .child(self.backup_primary(
                            "backup.import.vault.unlock",
                            if running {
                                strings.import_running
                            } else {
                                strings.import_unlock
                            },
                            Variant::Primary,
                            !running,
                            cx,
                            |this, cx| this.unlock_backup_vault(cx),
                        )),
                )
            }
            ImportStage::Applying => (
                body.child(self.backup_caption(strings.import_running, "backup.import.working")),
                footer,
            ),
            ImportStage::Preview(preview) => self.import_preview(preview, body, footer, cx),
            ImportStage::Applied {
                snapshot,
                changed,
                saved,
                vault,
                not_forgotten,
            } => {
                let mut text = match (snapshot, changed) {
                    (Some(path), true) => copy::fill(
                        strings.import_done_body,
                        &[("path", &super::home_relative(&path.display().to_string()))],
                    ),
                    _ => strings.import_done_nothing.to_owned(),
                };
                if *saved > 0 {
                    text.push(' ');
                    text.push_str(&copy::fill(
                        strings.import_done_secrets,
                        &[("count", &saved.to_string())],
                    ));
                }
                if *vault {
                    text.push(' ');
                    text.push_str(strings.import_done_vault);
                }
                let mut body =
                    body.child(self.backup_banner(Tone::Positive, text, "backup.import.applied"));
                if !not_forgotten.is_empty() {
                    body = body.child(self.backup_list(
                        Tone::Warning,
                        "backup.import.not-forgotten",
                        strings.import_not_forgotten.to_owned(),
                        not_forgotten.clone(),
                    ));
                }
                (
                    body,
                    footer.child(self.backup_primary(
                        "backup.import.done",
                        strings.close,
                        Variant::Primary,
                        true,
                        cx,
                        |this, cx| this.close_backup(cx),
                    )),
                )
            }
        };
        self.modal(
            window,
            "backup.import.dialog",
            560.0,
            ModalContent {
                header,
                subtitle,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// A notice with an introduction and one line per item.
    fn backup_list(
        &self,
        tone: Tone,
        selector: &'static str,
        intro: String,
        lines: Vec<String>,
    ) -> Div {
        let metrics = self.metrics();
        let mut list = components::banner(tone, metrics)
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .whitespace_normal()
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .child(SharedString::from(intro));
        for line in lines {
            list = list.child(
                div()
                    .text_size(metrics.caption_size())
                    .child(SharedString::from(line)),
            );
        }
        list
    }

    /// One line of the preview: what kind of record, and what happens to it.
    fn backup_count_row(&self, id: &'static str, label: &'static str, value: String) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE))
            .border_b_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(move || format!("backup.count.{id}"))
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(label),
            )
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(value)),
            )
    }

    fn change_text(&self, change: Change) -> String {
        let strings = self.backup_strings();
        if change.is_empty() {
            return strings.preview_unchanged.to_owned();
        }
        copy::fill(
            strings.preview_change,
            &[
                ("added", &change.added.to_string()),
                ("updated", &change.updated.to_string()),
                ("removed", &change.removed.to_string()),
            ],
        )
    }

    fn import_preview(
        &self,
        preview: &BackupPreview,
        body: Div,
        footer: Div,
        cx: &mut Context<Self>,
    ) -> (Div, Div) {
        let strings = self.backup_strings();
        let plan = &preview.plan;
        let document = &preview.document;
        let source = copy::fill(
            strings.import_source,
            &[
                ("platform", &platform_label(document.source_platform())),
                ("version", document.source_app_version()),
                (
                    "date",
                    &crate::app::hosts::format_unix(document.created_unix()),
                ),
            ],
        );
        let preferences = if plan.keymap_changed() {
            strings.preview_keymap_changed.to_owned()
        } else if plan.preferences_changed() {
            strings.preview_preferences_changed.to_owned()
        } else {
            strings.preview_unchanged.to_owned()
        };
        let mut secrets = match preview.secrets {
            Secrets::NotInFile => strings.preview_secrets_none.to_owned(),
            Secrets::Skipped => strings.preview_secrets_skipped.to_owned(),
            Secrets::Opened(_) => copy::fill(
                strings.preview_secrets_count,
                &[("count", &plan.secrets_to_import().to_string())],
            ),
        };
        let forgets = plan.secrets_to_forget();
        if !forgets.is_empty() {
            secrets.push_str(" \u{b7} ");
            secrets.push_str(&copy::fill(
                strings.preview_secrets_forget,
                &[("count", &forgets.len().to_string())],
            ));
        }
        // The vault part of a version 2 backup is restored as one piece with
        // the document; the preview says what it holds and that it replaces
        // this device's vault, which is the decision the confirm makes (R9).
        let vault_line = preview.vault.as_ref().map(|vault| {
            copy::fill(
                strings.preview_vault_line,
                &[
                    ("keys", &vault.content.keys().count().to_string()),
                    (
                        "passwords",
                        &vault.content.credentials().count().to_string(),
                    ),
                    ("trust", &vault.content.trust().entries().len().to_string()),
                ],
            )
        });
        let vault_note = preview
            .vault
            .as_ref()
            .map(|_| strings.preview_vault_replace.to_owned());
        let mut body = body
            .child(self.backup_caption(source, "backup.import.source"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w_0()
                    .debug_selector(|| "backup.import.counts".to_owned())
                    .child(self.backup_count_row(
                        "hosts",
                        strings.preview_hosts,
                        self.change_text(plan.hosts()),
                    ))
                    .child(self.backup_count_row(
                        "groups",
                        strings.preview_groups,
                        self.change_text(plan.groups()),
                    ))
                    .child(self.backup_count_row(
                        "forwards",
                        strings.preview_forwards,
                        self.change_text(plan.forwards()),
                    ))
                    .child(self.backup_count_row(
                        "snippets",
                        strings.preview_snippets,
                        self.change_text(plan.snippets()),
                    ))
                    .child(self.backup_count_row(
                        "known-hosts",
                        strings.preview_known_hosts,
                        self.change_text(plan.known_hosts()),
                    ))
                    .child(self.backup_count_row(
                        "preferences",
                        strings.preview_preferences,
                        preferences,
                    ))
                    .child(self.backup_count_row("secrets", strings.preview_secrets, secrets))
                    .when(vault_line.is_some(), |column| {
                        column.child(self.backup_count_row(
                            "vault",
                            strings.preview_vault,
                            vault_line.clone().unwrap_or_default(),
                        ))
                    }),
            );
        if let Some(note) = vault_note {
            body = body.child(self.backup_caption(note, "backup.import.vault.note"));
        }
        if !forgets.is_empty() {
            body = body.child(self.backup_list(
                Tone::Neutral,
                "backup.import.forget",
                strings.preview_forget_names.to_owned(),
                forgets.iter().map(|id| preview.name_of(*id)).collect(),
            ));
        }
        if !plan.trust_conflicts().is_empty() {
            body = body.child(
                self.backup_list(
                    Tone::Warning,
                    "backup.import.trust",
                    copy::fill(
                        strings.preview_trust_conflicts,
                        &[("count", &plan.trust_conflicts().len().to_string())],
                    ),
                    plan.trust_conflicts()
                        .iter()
                        .map(|conflict| {
                            copy::fill(
                                strings.preview_trust_line,
                                &[
                                    ("server", &format!("{}:{}", conflict.host, conflict.port)),
                                    ("local", &short_fingerprint(&conflict.local_fingerprint)),
                                    ("file", &short_fingerprint(&conflict.document_fingerprint)),
                                ],
                            )
                        })
                        .collect(),
                ),
            );
        }
        // The values that came from another kind of machine and are not already named
        // below as a key that is missing here: a key path that exists, a serial device.
        let named: HashSet<HostId> = preview.missing_keys.iter().map(|key| key.id).collect();
        let fallbacks: Vec<String> = plan
            .fallbacks()
            .iter()
            .filter(|fallback| {
                !(fallback.field == PlatformField::IdentityFile && named.contains(&fallback.id))
            })
            .map(|fallback| format!("{} \u{2014} {}", fallback.name, fallback.value))
            .collect();
        if !fallbacks.is_empty() {
            body = body.child(self.backup_list(
                Tone::Neutral,
                "backup.import.fallbacks",
                copy::fill(
                    strings.preview_fallbacks,
                    &[("platform", &platform_label(document.source_platform()))],
                ),
                fallbacks,
            ));
        }
        if !preview.missing_keys.is_empty() {
            let metrics = self.metrics();
            let source = platform_label(document.source_platform());
            let mut list = components::banner(Tone::Warning, metrics)
                .debug_selector(|| "backup.import.keys".to_owned())
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .whitespace_normal()
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .child(strings.preview_missing_keys);
            for key in &preview.missing_keys {
                let mut line = format!("{} \u{2014} {}", key.name, key.path);
                if key.foreign {
                    line.push_str(&format!(
                        " ({})",
                        copy::fill(strings.preview_key_from, &[("platform", &source)])
                    ));
                }
                list = list.child(
                    div()
                        .text_size(metrics.caption_size())
                        .child(SharedString::from(line)),
                );
            }
            body = body.child(list);
        }
        let mode = preview.mode;
        body = body
            .child(
                div()
                    .text_size(self.metrics().label_size())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(strings.mode_label),
            )
            .child(div().flex().child(self.segments(
                "backup.mode",
                vec![
                    (strings.mode_merge.to_owned(), mode == ApplyMode::Merge),
                    (strings.mode_replace.to_owned(), mode == ApplyMode::Replace),
                ],
                cx,
                |view, index, cx| {
                    let mode = if index == 0 {
                        ApplyMode::Merge
                    } else {
                        ApplyMode::Replace
                    };
                    view.set_backup_mode(mode, cx);
                },
            )))
            .child(self.backup_caption(
                match mode {
                    ApplyMode::Merge => strings.mode_merge_hint,
                    ApplyMode::Replace => strings.mode_replace_hint,
                },
                "backup.mode.hint",
            ));
        if plan.is_empty() {
            body = body.child(self.backup_banner(
                Tone::Neutral,
                strings.preview_nothing,
                "backup.import.nothing",
            ));
        }
        if preview.confirm_replace {
            body = body.child(self.backup_banner(
                Tone::Danger,
                strings.replace_confirm,
                "backup.import.confirm",
            ));
        }
        if let Some(problem) = &preview.problem {
            body = body.child(self.backup_banner(
                Tone::Danger,
                problem.clone(),
                "backup.import.problem",
            ));
        }
        let (label, variant) = match (mode, preview.confirm_replace) {
            (ApplyMode::Merge, _) => (strings.apply_merge, Variant::Primary),
            (ApplyMode::Replace, false) => (strings.apply_replace, Variant::Danger),
            (ApplyMode::Replace, true) => (strings.apply_replace_confirm, Variant::Danger),
        };
        let footer = footer
            .child(self.text_button(
                SharedString::from("backup.import.cancel"),
                strings.cancel,
                Variant::Ghost,
                cx,
                |this, cx| this.close_backup(cx),
            ))
            .child(self.backup_primary(
                "backup.import.apply",
                label,
                variant,
                true,
                cx,
                |this, cx| this.apply_backup(cx),
            ));
        (body, footer)
    }
}

// ---------------------------------------------------------------------------
// What the tests and the captures read
// ---------------------------------------------------------------------------

impl ShelluneView {
    /// Which backup dialog is open and at what stage, as an identifier.
    #[cfg(any(test, feature = "ui-capture"))]
    #[must_use]
    pub fn backup_stage(&self) -> Option<&'static str> {
        Some(match self.backup.as_ref()? {
            BackupDialog::Export(dialog) if dialog.report.is_some() => "export.done",
            BackupDialog::Export(_) => "export",
            BackupDialog::Import(dialog) => match dialog.stage {
                ImportStage::Refused(_) => "import.refused",
                ImportStage::Locked { .. } => "import.locked",
                ImportStage::VaultLocked { .. } => "import.vault-locked",
                ImportStage::Applying => "import.applying",
                ImportStage::Preview(_) => "import.preview",
                ImportStage::Applied { .. } => "import.applied",
            },
        })
    }

    /// Reads a backup file as if the file chooser had returned it.
    #[cfg(test)]
    pub fn read_backup_for_test(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.read_backup(path.to_path_buf(), cx);
    }

    /// The sentence the open backup dialog shows about a refusal or a problem.
    #[cfg(test)]
    #[must_use]
    pub fn backup_message_for_test(&self) -> Option<String> {
        match self.backup.as_ref()? {
            BackupDialog::Export(dialog) => dialog.failure.clone(),
            BackupDialog::Import(dialog) => match &dialog.stage {
                ImportStage::Refused(reason) => Some(reason.clone()),
                ImportStage::Locked { problem, .. } => problem.clone(),
                ImportStage::VaultLocked { problem, .. } => problem.clone(),
                ImportStage::Preview(preview) => preview.problem.clone(),
                ImportStage::Applied { .. } | ImportStage::Applying => None,
            },
        }
    }

    /// The connection counts of the preview and how many passwords it would save.
    #[cfg(test)]
    #[must_use]
    pub fn backup_preview_for_test(&self) -> Option<(Change, Change, Change, usize, Vec<String>)> {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_ref() else {
            return None;
        };
        let ImportStage::Preview(preview) = &dialog.stage else {
            return None;
        };
        Some((
            preview.plan.hosts(),
            preview.plan.groups(),
            preview.plan.snippets(),
            preview.plan.secrets_to_import(),
            preview
                .missing_keys
                .iter()
                .map(|key| key.path.clone())
                .collect(),
        ))
    }

    /// What the last export wrote: the file, how many passwords, the names whose
    /// password could not be read and the names whose password the store does not have.
    #[cfg(test)]
    #[must_use]
    pub fn export_report_for_test(&self) -> Option<ExportReportForTest> {
        let Some(BackupDialog::Export(dialog)) = self.backup.as_ref() else {
            return None;
        };
        let report = dialog.report.as_ref()?;
        Some((
            report.path.clone(),
            report.secrets,
            report.vault,
            report.unreadable.clone(),
            report.missing.clone(),
        ))
    }

    /// What the preview says beyond the counts: the passwords it removes from this
    /// machine (by name), the servers whose trusted key differs, and the values taken
    /// from another platform.
    #[cfg(test)]
    #[must_use]
    pub fn backup_notes_for_test(&self) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_ref() else {
            return None;
        };
        let ImportStage::Preview(preview) = &dialog.stage else {
            return None;
        };
        let plan = &preview.plan;
        Some((
            plan.secrets_to_forget()
                .iter()
                .map(|id| preview.name_of(*id))
                .collect(),
            plan.trust_conflicts()
                .iter()
                .map(|conflict| format!("{}:{}", conflict.host, conflict.port))
                .collect(),
            plan.fallbacks()
                .iter()
                .map(|fallback| fallback.value.clone())
                .collect(),
        ))
    }

    /// What the applied import reported: the passwords saved, and the names whose
    /// password the credential store would not remove.
    #[cfg(test)]
    #[must_use]
    pub fn backup_applied_for_test(&self) -> Option<(usize, Vec<String>)> {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_ref() else {
            return None;
        };
        let ImportStage::Applied {
            saved,
            not_forgotten,
            ..
        } = &dialog.stage
        else {
            return None;
        };
        Some((*saved, not_forgotten.clone()))
    }

    /// What the export dialog's two passphrase fields hold, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn export_passphrases_for_test(&self, cx: &App) -> Option<(String, String)> {
        let Some(BackupDialog::Export(dialog)) = self.backup.as_ref() else {
            return None;
        };
        Some((
            dialog.passphrase.read(cx).text().to_owned(),
            dialog.confirm.read(cx).text().to_owned(),
        ))
    }

    /// Why the export dialog refuses to start, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn export_refusal_for_test(&self, cx: &App) -> Option<&'static str> {
        let Some(BackupDialog::Export(dialog)) = self.backup.as_ref() else {
            return None;
        };
        self.export_refusal(dialog, cx)
    }

    /// Whether the import dialog's passphrase field has the keyboard, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn backup_field_focused_for_test(&self, window: &Window, cx: &App) -> bool {
        let Some(BackupDialog::Import(dialog)) = self.backup.as_ref() else {
            return false;
        };
        let ImportStage::Locked { field, .. } = &dialog.stage else {
            return false;
        };
        field.read(cx).focus_handle().is_focused(window)
    }

    /// The credential store this window keeps passwords in, for the tests.
    #[cfg(test)]
    #[must_use]
    pub fn secrets_for_test(&self) -> std::sync::Arc<dyn shellune_storage::SecretStore> {
        self.secrets.clone()
    }

    /// Opens the export dialog with the passwords included and a passphrase typed twice,
    /// for a capture. Nothing is written.
    #[cfg(feature = "ui-capture")]
    pub fn capture_backup_export(&mut self, cx: &mut Context<Self>) {
        self.settings_reveal(super::settings::Category::Data);
        self.open_backup_export(cx);
        self.toggle_backup_secrets(cx);
        if let Some(BackupDialog::Export(dialog)) = self.backup.as_ref() {
            let (first, second) = (dialog.passphrase.clone(), dialog.confirm.clone());
            first.update(cx, |field, cx| field.set_text("correct horse battery", cx));
            second.update(cx, |field, cx| field.set_text("correct horse battery", cx));
        }
        cx.notify();
    }

    /// Reads a backup over the settings' data category, for a capture: the preview of
    /// a real plan against the stores of the run. Nothing is applied.
    #[cfg(feature = "ui-capture")]
    pub fn capture_backup_import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.settings_reveal(super::settings::Category::Data);
        self.read_backup(path.to_path_buf(), cx);
    }

    /// The root of this window's stores, so a capture can put a fixture beside them.
    #[cfg(feature = "ui-capture")]
    #[must_use]
    pub fn capture_data_root(&self) -> PathBuf {
        self.store.root().to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::default_backup_name;

    #[test]
    fn the_default_file_name_carries_the_date() {
        // 2026-11-16 08:00 UTC.
        assert_eq!(
            default_backup_name(1_794_816_000),
            "shellune-backup-20261116.json"
        );
    }
}
