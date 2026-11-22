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

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
    HostReference, KdfParams, SecretStore, UnsealSource, VaultContent, VaultCredentialRecord,
    VaultKeyRecord, VaultStore, VaultStoreError,
};

use super::ShelluneView;
use super::shell::{ModalBehaviour, ModalContent};
use crate::app::components::{self, Variant, checkbox};
use crate::app::copy::{self, Locale, SecurityStrings};
use crate::app::icons::Icon;
use crate::app::text_field::TextField;
use crate::app::theme::{self, Metrics, Tone, palette, radius, spacing};
use crate::app::workspace::SectionStatus;

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

// ---------------------------------------------------------------------------
// The page's state
// ---------------------------------------------------------------------------

/// The settings page of the vault: its lists, its one dialog, and the state of
/// the one background job that may be running.
pub(crate) struct SecurityPage {
    /// The search that narrows both lists.
    search: Entity<TextField>,
    /// The vault as this window last read or wrote it, unlocked.
    ///
    /// `None` while no vault of this machine has been opened (or created) yet.
    /// Reads for one operation clone out of it; writes replace it with what the
    /// job saved.
    content: Option<VaultContent>,
    /// A vault job is running on the background executor.
    busy: bool,
    /// The vault is being opened at start-up; nothing may start meanwhile.
    warming: bool,
    /// Why the vault could not be opened at start-up, when it could not.
    open_problem: Option<String>,
    /// The notice of the last operation: its tone and its sentence, never a
    /// secret.
    notice: Option<(Tone, String)>,
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
    /// The vault's record versions only need a name that tells two writers
    /// apart; sync keeps its own device identity when it merges (ADR-5), which
    /// is a later task's wiring.
    device: String,
    /// Whether a test holds the finish of one job, to draw the busy state the
    /// window shows while work is in flight (the same shape as the sync
    /// panel's held finish).
    #[cfg(test)]
    hold_finish: bool,
    /// The held job result, until the test releases it.
    #[cfg(test)]
    held: Option<SecurityResult>,
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
}

impl SecurityPage {
    /// The page's fields, with its search created and nothing read yet.
    pub(crate) fn new(cx: &mut Context<ShelluneView>) -> Self {
        let placeholder = copy::security_for(Locale::current()).search_placeholder;
        let search = cx.new(|cx| {
            let mut field = TextField::new(placeholder, cx);
            field.set_leading_icon(Icon::Search, cx);
            field.set_clearable(true, cx);
            field
        });
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        Self {
            search,
            content: None,
            busy: false,
            warming: false,
            open_problem: None,
            notice: None,
            detail: None,
            revealed: false,
            dialog: None,
            rotation_public_key: None,
            job_book: None,
            dialog_scroll: ScrollHandle::new(),
            device: format!("app-{}", KeyId::new()),
            #[cfg(test)]
            hold_finish: false,
            #[cfg(test)]
            held: None,
        }
    }

    /// The vault as this window last read or wrote it, when it is unlocked.
    pub(crate) fn content(&self) -> Option<&VaultContent> {
        self.content.as_ref()
    }

    /// Whether a vault job or the start-up open is in flight.
    pub(crate) fn busy(&self) -> bool {
        self.busy || self.warming
    }
}

// ---------------------------------------------------------------------------
// What one background job did
// ---------------------------------------------------------------------------

/// What one vault job changed, in words the page can show without a secret.
pub(crate) enum SecurityReport {
    /// A key record was created (import or generation) or its metadata edited.
    KeySaved,
    /// A shared password record was created.
    CredentialSaved,
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
    ImportedWithWarning(String),
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
    created: SystemTime,
}

/// The immediate actions of the page and its detail cards.
#[derive(Clone, Copy, Debug)]
enum SecurityAction {
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

    /// The whole page: the actions, the search, the detail card of the chosen
    /// record and the two lists.
    pub(super) fn security_page(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let strings = self.security_strings();
        let mut page = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::BASE))
            .debug_selector(|| "security.page".to_owned());

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
        page = page.child(actions).child(
            div()
                .flex()
                .w_full()
                .min_w_0()
                .debug_selector(|| "security.search".to_owned())
                .child(self.security.search.clone()),
        );

        if let Some(record) = self.security.detail
            && let Some(card) = self.security_detail(record, cx)
        {
            page = page.child(card);
        }

        page = page
            .child(self.security_list(true, cx))
            .child(self.security_list(false, cx))
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(|| "security.secrets.hidden".to_owned())
                    .child(strings.secrets_hidden),
            );
        page
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
            let narrowed = !self.security.search.read(cx).text().trim().is_empty();
            return column.child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .debug_selector(move || format!("{selector}.empty"))
                    .child(if narrowed { strings.no_match } else { empty }),
            );
        }
        let selected = self.security.detail;
        let count = rows.len();
        let mut card = components::card(metrics).w_full().p(px(0.0)).gap(px(0.0));
        for (index, row) in rows.into_iter().enumerate() {
            let SecurityRow {
                record,
                name,
                caption,
                chip,
                hosts,
                created,
            } = row;
            let chosen = selected == Some(record);
            let row_selector = if keys {
                format!("security.key.{index}")
            } else {
                format!("security.credential.{index}")
            };
            let hosts_line = if hosts == 0 {
                strings.not_referenced.to_owned()
            } else {
                copy::fill(strings.hosts_count, &[("count", &hosts.to_string())])
            };
            let created_line = copy::fill(
                strings.created,
                &[(
                    "date",
                    &copy::relative_time(unix_of(created), remote_now(), locale),
                )],
            );
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
                    .when(chosen, |row| {
                        row.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .cursor_pointer()
                    .id(SharedString::from(row_selector.clone()))
                    .debug_selector({
                        let selector = row_selector.clone();
                        move || selector.clone()
                    })
                    .child(
                        components::accent_mark(chosen, metrics)
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
                                            .child(SharedString::from(name)),
                                    )
                                    .child(components::chip_within(
                                        SharedString::from(chip),
                                        Tone::Accent,
                                        metrics,
                                    )),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_MUTED))
                                    .child(SharedString::from(caption)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(SharedString::from(format!(
                                        "{hosts_line} · {created_line}"
                                    ))),
                            ),
                    )
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.security_pick(record, cx);
                    })),
            );
        }
        column.child(card)
    }

    /// The rows of one list, narrowed by the search.
    fn security_rows(&self, keys: bool, cx: &App) -> Vec<SecurityRow> {
        let Some(content) = self.security.content.as_ref() else {
            return Vec::new();
        };
        let strings = self.security_strings();
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

        match record {
            SecurityRecord::Key(id) => {
                let entry = content.key(id)?;
                let meta = entry.meta();
                let referencing = self.referencing_records(Some(id), None);
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
                    );
                if let Some(comment) = meta.comment().filter(|text| !text.trim().is_empty()) {
                    body = body.child(self.security_fact(
                        strings.label_comment,
                        comment.to_owned(),
                        None,
                    ));
                }
                body = body
                    .child(self.security_fact(
                        strings.label_algorithm,
                        meta.algorithm().to_owned(),
                        None,
                    ))
                    .child(self.security_fact(
                        strings.label_fingerprint,
                        meta.fingerprint().to_owned(),
                        Some("security.detail.fingerprint"),
                    ))
                    .child(self.security_fact(
                        strings.label_public_key,
                        meta.public_key().to_owned(),
                        Some("security.detail.public"),
                    ));
                card = card.child(body);
                card = card.child(self.security_references(&referencing, record, cx));
                card = card.child(self.security_actions_row(
                    &[
                        (SecurityAction::EditKey(id), strings.edit),
                        (SecurityAction::RotateKey(id), strings.rotate),
                        (SecurityAction::ExportKey(id), strings.export_choose),
                        (SecurityAction::DeleteKey(id), strings.delete),
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
                    );
                if let Some(user) = meta.user().filter(|text| !text.trim().is_empty()) {
                    // Copyable the way the fingerprint and the public key are:
                    // a login user name is what a person pastes into the next
                    // form (R1).
                    body = body.child(self.security_fact(
                        strings.label_user,
                        user.to_owned(),
                        Some("security.detail.user"),
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
                if let Some(text) = value {
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
                        password_row = password_row.child(
                            components::compact_button(strings.show, Variant::Ghost, metrics)
                                .id("security.detail.reveal")
                                .debug_selector(|| "security.detail.reveal".to_owned())
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.security.revealed = true;
                                    cx.notify();
                                })),
                        );
                    }
                    let strings_copy = strings.copy_value;
                    password_row = password_row.child(
                        div()
                            .debug_selector(|| "security.detail.copy.password".to_owned())
                            .child(components::copy_on_click(
                                div()
                                    .flex_none()
                                    .text_size(metrics.caption_size())
                                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                                    .child(strings_copy),
                                text,
                            )),
                    );
                }
                body = body.child(password_row);
                card = card.child(body);
                card = card.child(self.security_references(&referencing, record, cx));
                card = card.child(self.security_actions_row(
                    &[
                        (SecurityAction::EditCredential(id), strings.edit),
                        (SecurityAction::DuplicateCredential(id), strings.duplicate),
                        (SecurityAction::DeleteCredential(id), strings.delete),
                    ],
                    busy,
                    cx,
                ));
            }
        }
        Some(card)
    }

    /// One fact line of a detail card, copyable when it is long and public.
    fn security_fact(
        &self,
        label: &'static str,
        value: String,
        selector: Option<&'static str>,
    ) -> Div {
        let metrics = self.metrics();
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
        // A fingerprint and a public key are what a person copies elsewhere;
        // the comment and the user name are short enough to select by hand.
        let line = if selector.is_some() {
            components::copy_on_click(line, value.clone()).child(SharedString::from(value))
        } else {
            line.child(SharedString::from(value))
        };
        column.child(div().flex().items_center().min_w_0().child(line))
    }

    /// The connections that reference a record, each with its unbind action.
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
        actions: &[(SecurityAction, &'static str)],
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
        for (index, (action, label)) in actions.iter().copied().enumerate() {
            let selector = format!("security.detail.action.{index}");
            let reserved = components::compact_label_reserved_width(label, metrics);
            let button = components::compact_button(label, Variant::Secondary, metrics)
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
            row = row.child(if busy {
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
    fn security_start(&mut self, action: SecurityAction, cx: &mut Context<Self>) {
        if self.security.busy() || self.security.dialog.is_some() {
            return;
        }
        let strings = self.security_strings();
        match action {
            SecurityAction::Import => {
                let pem = cx.new(|cx| {
                    let mut field = TextField::new(strings.import_pem, cx);
                    field.set_keeps_line_breaks(true, cx);
                    field
                });
                let passphrase = self.security_secret_field(cx);
                let name = cx.new(|cx| TextField::new(strings.import_name, cx));
                self.security.dialog = Some(SecurityDialog::ImportKey {
                    pem,
                    passphrase,
                    name,
                    refusal: None,
                });
            }
            SecurityAction::Generate => {
                let name = cx.new(|cx| TextField::new(strings.generate_name, cx));
                let comment = cx.new(|cx| TextField::new(strings.generate_comment, cx));
                self.security.dialog = Some(SecurityDialog::GenerateKey {
                    kind: keys_api::ManagedKeyKind::Ed25519,
                    name,
                    comment,
                    refusal: None,
                });
            }
            SecurityAction::NewCredential => {
                let name = cx.new(|cx| TextField::new(strings.credential_name, cx));
                let user = cx.new(|cx| TextField::new(strings.credential_user, cx));
                let secret = self.security_secret_field(cx);
                let length =
                    cx.new(|cx| TextField::with_text("", PASSWORD_LENGTH_DEFAULT.to_string(), cx));
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
                let Some(entry) = self
                    .security
                    .content
                    .as_ref()
                    .and_then(|vault| vault.key(id))
                else {
                    return;
                };
                let meta = entry.meta();
                let name = cx.new(|cx| TextField::with_text("", meta.name(), cx));
                let comment =
                    cx.new(|cx| TextField::with_text("", meta.comment().unwrap_or_default(), cx));
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
                let name = cx.new(|cx| TextField::with_text("", format!("{previous_name} 2"), cx));
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
                let Some(entry) = self
                    .security
                    .content
                    .as_ref()
                    .and_then(|vault| vault.credential(id))
                else {
                    return;
                };
                let meta = entry.meta();
                let name = cx.new(|cx| TextField::with_text("", meta.name(), cx));
                let user =
                    cx.new(|cx| TextField::with_text("", meta.user().unwrap_or_default(), cx));
                let secret = self.security_secret_field(cx);
                self.security.dialog = Some(SecurityDialog::EditCredential {
                    credential: id,
                    name,
                    user,
                    secret,
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

    /// A masked, revealable field for a secret a dialog asks for.
    fn security_secret_field(&self, cx: &mut Context<Self>) -> Entity<TextField> {
        let strings = self.security_strings();
        let field = cx.new(|cx| {
            let mut field = TextField::new(strings.credential_password, cx);
            field.set_masked(true, cx);
            field.set_revealable(true, cx);
            field
        });
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        field
    }

    fn security_key_missing(&self, id: KeyId) -> bool {
        self.security
            .content
            .as_ref()
            .and_then(|vault| vault.key(id))
            .is_none()
    }

    /// Chooses the record whose detail card is drawn.
    fn security_pick(&mut self, record: SecurityRecord, cx: &mut Context<Self>) {
        self.security.detail = Some(record);
        // A reveal belongs to the record it was asked for, never to the next.
        self.security.revealed = false;
        cx.notify();
    }

    /// Closes the page's dialog; a job in flight keeps it until it ends.
    fn security_close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.security.busy {
            return;
        }
        self.security.dialog = None;
        self.security.rotation_public_key = None;
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
            |dialog| {
                if let SecurityDialog::NewCredential {
                    letters,
                    digits,
                    symbols,
                    ..
                } = dialog
                {
                    match which {
                        0 => *letters = !*letters,
                        1 => *digits = !*digits,
                        _ => *symbols = !*symbols,
                    }
                }
            },
            cx,
        );
    }

    /// Fills the new-password field with a generated value, so the value is
    /// visible, editable and refused as a whole when no class remains.
    fn security_generate_password(&mut self, cx: &mut Context<Self>) {
        let Some(SecurityDialog::NewCredential {
            secret,
            length,
            letters,
            digits,
            symbols,
            ..
        }) = self.security.dialog.as_ref()
        else {
            return;
        };
        let length = length
            .read(cx)
            .text()
            .trim()
            .parse::<usize>()
            .ok()
            .unwrap_or(PASSWORD_LENGTH_DEFAULT)
            .clamp(PASSWORD_LENGTH_MIN, PASSWORD_LENGTH_MAX);
        let value = generate_password(length, *letters, *digits, *symbols);
        let field = secret.clone();
        field.update(cx, |field, cx| field.set_text(value, cx));
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
                        content
                            .insert_key(VaultKeyRecord::new(
                                ManagedKeyMeta::new(
                                    KeyId::new(),
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
                            report: SecurityReport::KeySaved,
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
                            report: SecurityReport::KeySaved,
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
                        content
                            .insert_credential(VaultCredentialRecord::new(
                                CredentialMeta::new(
                                    CredentialId::new(),
                                    name,
                                    (!user.is_empty()).then(|| user.clone()),
                                    VaultRecordVersion::new(1, device),
                                ),
                                SecretString::new(password),
                            ))
                            .map_err(|error| error.to_string())?;
                        Ok(SecurityChange::Save {
                            report: SecurityReport::CredentialSaved,
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
        }
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
    fn security_set_notice(&mut self, tone: Tone, text: String, cx: &mut Context<Self>) {
        self.security.notice = Some((tone, text));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // The background job
    // -----------------------------------------------------------------------

    /// Runs one vault edit on the background executor: the page turns busy,
    /// the window keeps drawing, and the outcome arrives as a notice.
    fn security_run(&mut self, edit: SecurityEdit, cx: &mut Context<Self>) {
        let store = VaultStore::at(self.store.root());
        let secrets = self.secrets.clone();
        let hosts = self.book.hosts().to_vec();
        let connections = self.book.connections().to_vec();
        self.security.busy = true;
        self.security.notice = None;
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
        let strings = self.security_strings();
        match result {
            SecurityResult::Applied {
                content,
                previous,
                records,
                changed,
                report,
            } => {
                self.security.content = Some(content);
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
        let records = self.book.records();
        content.set_references(&HostReference::snapshot_records(&records));
        self.security.busy = true;
        let work = cx.background_executor().spawn(async move {
            store
                .save_records(
                    &content,
                    &records,
                    &[UnsealSource::LocalMachine],
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
                    Ok(content) => view.security.content = Some(content),
                    Err(reason) => view.security_set_notice(
                        Tone::Danger,
                        format!("{}: {reason}", view.security_strings().vault_failed),
                        cx,
                    ),
                }
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
        let records = self.book.records();
        self.security.busy = true;
        let work = cx.background_executor().spawn(async move {
            store
                .save_records(
                    &previous,
                    &records,
                    &[UnsealSource::LocalMachine],
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
                    Ok(content) => view.security.content = Some(content),
                    Err(reason) => view.security_set_notice(
                        Tone::Danger,
                        format!("{}: {reason}", view.security_strings().vault_failed),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens this machine's vault at start-up, so a connection that needs it
    /// does not wait for the settings page to be visited first. A machine with
    /// no vault yet stays without one until the first record creates it.
    pub(super) fn vault_warm(&mut self, cx: &mut Context<Self>) {
        if self.security.content.is_some() || self.security.warming {
            return;
        }
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
                view.security.warming = false;
                match opened {
                    Ok(Some(content)) => view.security.content = Some(content),
                    Ok(None) => {}
                    Err(reason) => view.security.open_problem = Some(reason),
                }
                cx.notify();
            });
        })
        .detach();
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
                let body = self
                    .security_dialog_body("security.dialog.import", cx)
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
                let footer = self.security_dialog_footer(strings.import_key, cx);
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
                let footer = self.security_dialog_footer(strings.generate_key, cx);
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
                let footer = self.security_dialog_footer(strings.save, cx);
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
                let footer = self.security_dialog_footer(strings.export_choose, cx);
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
                let footer = self.security_dialog_footer(strings.rotate, cx);
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
                let footer = self.security_dialog_footer(strings.delete, cx);
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
                let footer = self.security_dialog_footer(strings.save, cx);
                (
                    strings.new_credential_title.to_owned(),
                    None,
                    Icon::Lock,
                    body,
                    footer,
                )
            }
            SecurityDialog::EditCredential { refusal, .. } => {
                let body = self
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
                    ))
                    .children(
                        refusal
                            .clone()
                            .map(|reason| security_refusal_banner(reason, metrics)),
                    );
                let footer = self.security_dialog_footer(strings.save, cx);
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
                let footer = self.security_dialog_footer(strings.delete, cx);
                (
                    strings.delete_credential_title.to_owned(),
                    None,
                    Icon::Trash,
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
    fn security_dialog_footer(&self, primary: &'static str, cx: &mut Context<Self>) -> Div {
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
                    Variant::Primary,
                    cx,
                    |this, cx| this.security_submit(cx),
                )
                .h(metrics.control_height()),
            )
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
        SecurityDialog::NewCredential { length, .. } => Some(length),
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
                }
            }
            // The reference snapshot and the catalog checks run inside the
            // save: a record pointing at nothing, or a vault whose mirror
            // disagrees with the book, is refused before any file is touched.
            content.set_references(&HostReference::snapshot_records(&all));
            if let Err(error) = store.save_records(
                &content,
                &all,
                &[UnsealSource::LocalMachine],
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

/// The localized sentence of one report, and the tone it is drawn in.
fn security_report_text(report: &SecurityReport, strings: &SecurityStrings) -> (Tone, String) {
    match report {
        SecurityReport::KeySaved => (Tone::Positive, strings.key_saved.to_owned()),
        SecurityReport::CredentialSaved => (Tone::Positive, strings.credential_created.to_owned()),
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
        SecurityReport::ImportedWithWarning(warning) => (Tone::Warning, warning.clone()),
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
    let meta = ManagedKeyMeta::new(
        KeyId::new(),
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
            report: SecurityReport::ImportedWithWarning(warning),
            records: Vec::new(),
        },
        None => SecurityChange::Save {
            report: SecurityReport::KeySaved,
            records: Vec::new(),
        },
    })
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
