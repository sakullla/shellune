//! Saved forwarding rules and the listeners a connection is actually running (R11).
//!
//! The book is configuration. A rule is listening only after this center has been
//! told that a bind or a remote listen succeeded. Loading the book, copying a rule
//! and failing to start one do not open a socket.

use std::collections::HashMap;

use shellune_core::{
    Endpoint, ForwardBook, ForwardId, ForwardKind, ForwardRule, HostId, SessionId,
};
use shellune_ssh::{ForwardCommand, ForwardEvent, ForwardFailure, ForwardLink, ForwardSpec};

/// Which rules the forward page is listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardScope {
    /// Every saved rule.
    All,
    /// Rules for one saved host.
    Host(HostId),
}

/// What a rule is doing, apart from the configuration that was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardPhase {
    /// Saved, and not started.
    Saved,
    /// A start was sent and has not been confirmed.
    Starting {
        /// Which start.
        generation: u64,
        /// The connection that was asked.
        session: SessionId,
    },
    /// The listener is up.
    Listening {
        /// Which start.
        generation: u64,
        /// The connection carrying it.
        session: SessionId,
        /// Address that is listening.
        host: String,
        /// Port that is listening.
        port: u16,
    },
    /// The last start failed. The saved rule is unchanged.
    Failed {
        /// Which start.
        generation: u64,
        /// Why it is not listening.
        failure: ForwardFailure,
    },
    /// The user stopped it.
    Stopped,
}

impl ForwardPhase {
    fn generation(&self) -> Option<u64> {
        match self {
            Self::Starting { generation, .. }
            | Self::Listening { generation, .. }
            | Self::Failed { generation, .. } => Some(*generation),
            Self::Saved | Self::Stopped => None,
        }
    }

    fn session(&self) -> Option<SessionId> {
        match self {
            Self::Starting { session, .. } | Self::Listening { session, .. } => Some(*session),
            _ => None,
        }
    }

    /// Whether this phase is a listener the user would lose by closing the carrier.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Listening { .. })
    }
}

/// One rule as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRow {
    /// Saved rule.
    pub rule: ForwardRule,
    /// Runtime, which is not stored in the rule file.
    pub phase: ForwardPhase,
    /// A later connection that failed while the listener stayed up.
    pub attempt: Option<ForwardFailure>,
}

/// The editor's choices. The text fields live on the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardEditor {
    /// `None` while creating a rule.
    pub id: Option<ForwardId>,
    /// Saved host that would carry the tunnel.
    pub host: Option<HostId>,
    /// Local, remote or dynamic.
    pub kind: ForwardKind,
    /// Why the last save was refused. The form stays open.
    pub error: Option<String>,
}

/// What starting a rule still needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardStart {
    /// The listen address is not loopback. Nothing has been bound.
    Exposure {
        /// The rule.
        id: ForwardId,
        /// Address that would be exposed.
        listen: String,
    },
    /// No authenticated connection is available yet.
    Connection {
        /// The rule to start once that connection exists.
        id: ForwardId,
        /// The host to connect to.
        host: HostId,
    },
    /// The connection can start this spec now.
    Ready {
        /// Session that already has a forward link.
        session: SessionId,
        /// Spec to send.
        spec: ForwardSpec,
    },
}

/// Rules, their runtime, and the connections that can carry them.
pub struct ForwardCenter {
    book: ForwardBook,
    phase: HashMap<ForwardId, ForwardPhase>,
    attempt: HashMap<ForwardId, ForwardFailure>,
    generation: HashMap<ForwardId, u64>,
    carriers: HashMap<SessionId, ForwardLink>,
    scope: ForwardScope,
    editor: Option<ForwardEditor>,
    /// A non-loopback start the user has confirmed and not yet consumed.
    exposure: Option<ForwardId>,
    /// Why the rule file could not be read. The book is left as it was.
    load_error: Option<String>,
}

impl ForwardCenter {
    /// No rules and no listeners.
    #[must_use]
    pub fn new() -> Self {
        Self {
            book: ForwardBook::new(),
            phase: HashMap::new(),
            attempt: HashMap::new(),
            generation: HashMap::new(),
            carriers: HashMap::new(),
            scope: ForwardScope::All,
            editor: None,
            exposure: None,
            load_error: None,
        }
    }

    /// Why the last read of the rule file failed.
    #[must_use]
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Records a read failure. Nothing is bound.
    pub fn note_load_error(&mut self, error: String) {
        self.load_error = Some(error);
    }

    /// Clears a read failure after a successful load.
    pub fn clear_load_error(&mut self) {
        self.load_error = None;
    }

    /// Replaces the saved book. Listeners that are already up are left alone.
    pub fn replace_book(&mut self, book: ForwardBook) {
        let ids: Vec<ForwardId> = self.phase.keys().copied().collect();
        for id in ids {
            if book.rule(id).is_none() {
                self.phase.remove(&id);
                self.attempt.remove(&id);
                self.generation.remove(&id);
            }
        }
        self.book = book;
    }

    /// Saved rules, unfiltered.
    #[must_use]
    pub fn book(&self) -> &ForwardBook {
        &self.book
    }

    /// The page filter.
    pub fn set_scope(&mut self, scope: ForwardScope) {
        self.scope = scope;
    }

    /// Rows in saved order, after the filter.
    #[must_use]
    pub fn rows(&self) -> Vec<ForwardRow> {
        self.book
            .rules()
            .iter()
            .filter(|rule| self.matches(rule))
            .map(|rule| ForwardRow {
                rule: rule.clone(),
                phase: self
                    .phase
                    .get(&rule.id())
                    .cloned()
                    .unwrap_or(ForwardPhase::Saved),
                attempt: self.attempt.get(&rule.id()).cloned(),
            })
            .collect()
    }

    fn matches(&self, rule: &ForwardRule) -> bool {
        match self.scope {
            ForwardScope::All => true,
            ForwardScope::Host(host) => rule.host() == host,
        }
    }

    /// Opens a new-rule form. Nothing is saved and nothing is bound.
    pub fn begin_new(&mut self, host: Option<HostId>) {
        self.editor = Some(ForwardEditor {
            id: None,
            host,
            kind: ForwardKind::Local,
            error: None,
        });
    }

    /// Opens the form on a saved rule.
    pub fn begin_edit(&mut self, id: ForwardId) {
        let Some(rule) = self.book.rule(id).cloned() else {
            return;
        };
        self.editor = Some(ForwardEditor {
            id: Some(id),
            host: Some(rule.host()),
            kind: rule.kind(),
            error: None,
        });
    }

    /// The open form.
    #[must_use]
    pub fn editor(&self) -> Option<&ForwardEditor> {
        self.editor.as_ref()
    }

    /// The open form.
    pub fn editor_mut(&mut self) -> Option<&mut ForwardEditor> {
        self.editor.as_mut()
    }

    /// Closes the form without writing.
    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    /// A book with the form applied, or why the form is not a rule yet.
    ///
    /// The center's book is unchanged, so a failed write keeps the previous file
    /// and the form.
    pub fn preview(
        &self,
        listen_host: &str,
        listen_port: &str,
        target_host: &str,
        target_port: &str,
    ) -> Result<ForwardBook, String> {
        let editor = self.editor.as_ref().ok_or("no forward is being edited")?;
        let host = editor.host.ok_or("choose the host this forward uses")?;
        let listen = endpoint(listen_host, listen_port, true)?;
        let target = if editor.kind == ForwardKind::Dynamic {
            None
        } else {
            Some(endpoint(target_host, target_port, false)?)
        };
        let rule = match editor.id {
            Some(id) => rule_with_id(id, host, editor.kind, listen, target)?,
            None => new_rule(host, editor.kind, listen, target)?,
        };
        let mut book = self.book.clone();
        if editor.id.is_some() {
            book.update(rule).map_err(|error| error.to_string())?;
        } else {
            book.insert(rule).map_err(|error| error.to_string())?;
        }
        Ok(book)
    }

    /// Stores a book that was just written to disk.
    ///
    /// Kind, host and endpoints are the saved shape. Changing that shape while
    /// the rule is listening or starting stops that generation first, so the new
    /// configuration is not shown as the listener that is still bound. A save
    /// that keeps the shape leaves the listener up. A non-loopback confirmation
    /// does not carry over to a different shape.
    pub fn adopt(&mut self, book: ForwardBook) {
        let changed: Vec<ForwardId> = book
            .rules()
            .iter()
            .filter(|rule| {
                self.book
                    .rule(rule.id())
                    .is_some_and(|previous| rule_shape_changed(previous, rule))
            })
            .map(ForwardRule::id)
            .collect();
        for id in changed {
            if self.exposure == Some(id) {
                self.exposure = None;
            }
            if self.phase.get(&id).is_some_and(ForwardPhase::is_active) {
                let _ = self.stop_one(id);
            }
        }
        self.replace_book(book);
        self.editor = None;
    }

    /// Remembers why the form could not be saved.
    pub fn fail_editor(&mut self, error: String) {
        if let Some(editor) = self.editor.as_mut() {
            editor.error = Some(error);
        }
    }

    /// A copy of a rule, not listening, ready to insert.
    #[must_use]
    pub fn copy_of(&self, id: ForwardId) -> Option<ForwardRule> {
        self.book.rule(id).map(ForwardRule::copied)
    }

    /// The book without one rule. The center does not change until [`Self::adopt`].
    pub fn without(&self, id: ForwardId) -> Result<ForwardBook, String> {
        let mut book = self.book.clone();
        book.remove(id).map_err(|error| error.to_string())?;
        Ok(book)
    }

    /// Active rules on one session, for the close question.
    #[must_use]
    pub fn active_on(&self, session: SessionId) -> Vec<ForwardRule> {
        self.book
            .rules()
            .iter()
            .filter(|rule| {
                self.phase
                    .get(&rule.id())
                    .is_some_and(|phase| phase.is_active() && phase.session() == Some(session))
            })
            .cloned()
            .collect()
    }

    /// Every rule that is listening or starting, ignoring the page filter.
    #[must_use]
    pub fn active_rules(&self) -> Vec<ForwardRule> {
        self.book
            .rules()
            .iter()
            .filter(|rule| {
                self.phase
                    .get(&rule.id())
                    .is_some_and(ForwardPhase::is_active)
            })
            .cloned()
            .collect()
    }

    /// Whether any rule is listening or starting.
    #[must_use]
    pub fn any_active(&self) -> bool {
        self.phase.values().any(ForwardPhase::is_active)
    }

    /// Whether this session has a forward link.
    #[must_use]
    pub fn has_carrier(&self, session: SessionId) -> bool {
        self.carriers.contains_key(&session)
    }

    /// Whether any link is still being polled.
    #[must_use]
    pub fn has_carriers(&self) -> bool {
        !self.carriers.is_empty()
    }

    /// Attaches the forward link of a connection that just authenticated.
    pub fn attach(&mut self, session: SessionId, link: ForwardLink) {
        self.carriers.insert(session, link);
    }

    /// Drops the link after the session is gone.
    pub fn detach(&mut self, session: SessionId) {
        self.carriers.remove(&session);
        self.mark_carrier_lost(session);
    }

    /// Decides whether a start may proceed.
    ///
    /// `connected` is a session for that host that already has a link.
    pub fn prepare_start(
        &mut self,
        id: ForwardId,
        connected: Option<SessionId>,
    ) -> Result<ForwardStart, String> {
        let rule = self
            .book
            .rule(id)
            .cloned()
            .ok_or("that forward is not saved")?;
        if rule.needs_exposure_confirmation() && self.exposure.take() != Some(id) {
            return Ok(ForwardStart::Exposure {
                id,
                listen: rule.listen().to_string(),
            });
        }
        let Some(session) = connected.filter(|session| self.carriers.contains_key(session)) else {
            return Ok(ForwardStart::Connection {
                id,
                host: rule.host(),
            });
        };
        let generation = self.generation.entry(id).or_insert(0);
        *generation = generation.saturating_add(1);
        let generation = *generation;
        self.phase.insert(
            id,
            ForwardPhase::Starting {
                generation,
                session,
            },
        );
        self.attempt.remove(&id);
        Ok(ForwardStart::Ready {
            session,
            spec: spec_of(&rule, generation),
        })
    }

    /// Records that the user confirmed a non-loopback listen. Nothing is bound.
    pub fn confirm_exposure(&mut self, id: ForwardId) {
        self.exposure = Some(id);
    }

    /// Forgets a confirmation the user cancelled.
    pub fn cancel_exposure(&mut self) {
        self.exposure = None;
    }

    /// Sends a command on one connection. False means that connection is gone.
    pub fn send(&mut self, session: SessionId, command: ForwardCommand) -> bool {
        let Some(link) = self.carriers.get_mut(&session) else {
            return false;
        };
        link.send(command)
    }

    /// Sessions that currently hold a forward link.
    #[must_use]
    pub fn carrier_sessions(&self) -> Vec<SessionId> {
        self.carriers.keys().copied().collect()
    }

    /// Stops one rule. Other rules on the same connection stay as they are.
    pub fn stop_one(&mut self, id: ForwardId) -> bool {
        let session = self.phase.get(&id).and_then(ForwardPhase::session);
        let active = self.phase.get(&id).is_some_and(ForwardPhase::is_active);
        self.phase.insert(id, ForwardPhase::Stopped);
        if active && let Some(session) = session {
            return self.send(session, ForwardCommand::Stop { id });
        }
        false
    }

    /// Marks rules the user stopped on this session and returns the stop commands.
    pub fn stop_session(&mut self, session: SessionId) -> Vec<ForwardId> {
        let ids: Vec<ForwardId> = self
            .phase
            .iter()
            .filter_map(|(id, phase)| {
                (phase.is_active() && phase.session() == Some(session)).then_some(*id)
            })
            .collect();
        for id in &ids {
            let generation = self.phase.get(id).and_then(ForwardPhase::generation);
            self.phase.insert(*id, ForwardPhase::Stopped);
            if let Some(generation) = generation {
                let _ = self.send(session, ForwardCommand::Stop { id: *id });
                let _ = generation;
            }
        }
        ids
    }

    /// Applies events. A stale generation is ignored. A lost carrier is not left
    /// looking like it is still listening.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        let sessions: Vec<SessionId> = self.carriers.keys().copied().collect();
        for session in sessions {
            let Some(link) = self.carriers.get_mut(&session) else {
                continue;
            };
            let events = link.drain();
            let open = link.events_open();
            if !events.is_empty() {
                changed = true;
            }
            for event in events {
                self.apply(session, event);
            }
            if !open {
                self.mark_carrier_lost(session);
                self.carriers.remove(&session);
                changed = true;
            }
        }
        changed
    }

    fn apply(&mut self, session: SessionId, event: ForwardEvent) {
        match event {
            ForwardEvent::Listening {
                id,
                generation,
                host,
                port,
            } => {
                // Stopped stays stopped until a later start sets a new generation.
                // A late success of the generation that was just stopped must not
                // put the old bind back on screen.
                if matches!(self.phase.get(&id), Some(ForwardPhase::Stopped)) {
                    return;
                }
                if !self.current(id, generation) {
                    return;
                }
                self.phase.insert(
                    id,
                    ForwardPhase::Listening {
                        generation,
                        session,
                        host,
                        port,
                    },
                );
            }
            ForwardEvent::Failed {
                id,
                generation,
                failure,
            } => {
                if !self.current(id, generation) && !matches!(failure, ForwardFailure::CarrierLost)
                {
                    return;
                }
                if matches!(self.phase.get(&id), Some(ForwardPhase::Stopped)) {
                    return;
                }
                self.phase.insert(
                    id,
                    ForwardPhase::Failed {
                        generation,
                        failure,
                    },
                );
            }
            ForwardEvent::AttemptFailed {
                id,
                generation,
                failure,
            } => {
                if self.current(id, generation) {
                    self.attempt.insert(id, failure);
                }
            }
            ForwardEvent::Stopped { id, generation } => {
                if self.current(id, generation) {
                    self.phase.insert(id, ForwardPhase::Stopped);
                }
            }
            ForwardEvent::CarrierLost => self.mark_carrier_lost(session),
        }
    }

    fn current(&self, id: ForwardId, generation: u64) -> bool {
        self.phase
            .get(&id)
            .and_then(ForwardPhase::generation)
            .is_none_or(|current| current == generation)
    }

    /// Marks one saved rule as listening, for a test that cannot bind a socket.
    #[cfg(test)]
    pub fn note_listening_for_test(
        &mut self,
        id: ForwardId,
        session: SessionId,
        host: &str,
        port: u16,
    ) {
        self.phase.insert(
            id,
            ForwardPhase::Listening {
                generation: 1,
                session,
                host: host.to_owned(),
                port,
            },
        );
    }

    /// Attaches a link whose commands the caller can read. Nothing is bound.
    #[cfg(test)]
    pub fn attach_observed_for_test(
        &mut self,
        session: SessionId,
    ) -> shellune_ssh::forwarding::ObservedForwards {
        let (link, observed) = ForwardLink::observe();
        self.attach(session, link);
        observed
    }

    /// Whether the rule is in the listening phase.
    #[cfg(test)]
    pub fn is_listening_for_test(&self, id: ForwardId) -> bool {
        matches!(self.phase.get(&id), Some(ForwardPhase::Listening { .. }))
    }

    fn mark_carrier_lost(&mut self, session: SessionId) {
        let ids: Vec<ForwardId> = self
            .phase
            .iter()
            .filter_map(|(id, phase)| (phase.session() == Some(session)).then_some(*id))
            .collect();
        for id in ids {
            if matches!(self.phase.get(&id), Some(ForwardPhase::Stopped)) {
                continue;
            }
            let generation = self
                .phase
                .get(&id)
                .and_then(ForwardPhase::generation)
                .unwrap_or(0);
            self.phase.insert(
                id,
                ForwardPhase::Failed {
                    generation,
                    failure: ForwardFailure::CarrierLost,
                },
            );
        }
    }
}

impl Default for ForwardCenter {
    fn default() -> Self {
        Self::new()
    }
}

fn endpoint(host: &str, port: &str, listen: bool) -> Result<Endpoint, String> {
    let host = host.trim();
    let host = if host.is_empty() && listen {
        "127.0.0.1"
    } else {
        host
    };
    let port = port.trim();
    let port: u16 = port.parse().map_err(|_| {
        format!("port {port:?} is not a number from 1 to 65535. The rule was not saved.")
    })?;
    Endpoint::new(host, port).map_err(|error| error.to_string())
}

fn new_rule(
    host: HostId,
    kind: ForwardKind,
    listen: Endpoint,
    target: Option<Endpoint>,
) -> Result<ForwardRule, String> {
    match kind {
        ForwardKind::Local => Ok(ForwardRule::local(
            host,
            listen,
            target.ok_or("a local forward needs a target")?,
        )),
        ForwardKind::Remote => Ok(ForwardRule::remote(
            host,
            listen,
            target.ok_or("a remote forward needs a target")?,
        )),
        ForwardKind::Dynamic => Ok(ForwardRule::dynamic(host, listen)),
    }
}

fn rule_shape_changed(previous: &ForwardRule, next: &ForwardRule) -> bool {
    previous.kind() != next.kind()
        || previous.host() != next.host()
        || previous.listen() != next.listen()
        || previous.target() != next.target()
}

fn rule_with_id(
    id: ForwardId,
    host: HostId,
    kind: ForwardKind,
    listen: Endpoint,
    target: Option<Endpoint>,
) -> Result<ForwardRule, String> {
    ForwardRule::rebuild(id, host, kind, listen, target).map_err(|error| error.to_string())
}

fn spec_of(rule: &ForwardRule, generation: u64) -> ForwardSpec {
    ForwardSpec {
        id: rule.id(),
        generation,
        kind: rule.kind(),
        listen_host: rule.listen().host().to_owned(),
        listen_port: rule.listen().port().get(),
        target_host: rule.target().map(|endpoint| endpoint.host().to_owned()),
        target_port: rule.target().map(|endpoint| endpoint.port().get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostId {
        HostId::new()
    }

    fn local(host: HostId, port: u16) -> ForwardRule {
        ForwardRule::local(
            host,
            Endpoint::loopback(port).expect("port"),
            Endpoint::new("10.0.0.8", 80).expect("target"),
        )
    }

    #[test]
    fn loading_a_book_does_not_make_a_rule_active() {
        let rule = local(host(), 4400);
        let mut book = ForwardBook::new();
        book.insert(rule.clone()).expect("insert");
        let mut center = ForwardCenter::new();
        center.replace_book(book);
        assert!(!center.any_active());
        assert!(center.carriers.is_empty());
        assert_eq!(center.rows()[0].phase, ForwardPhase::Saved);
        assert_eq!(center.rows()[0].rule.id(), rule.id());
    }

    #[test]
    fn a_non_loopback_listen_asks_before_it_can_start() {
        let id_host = host();
        let rule = ForwardRule::local(
            id_host,
            Endpoint::new("0.0.0.0", 4401).expect("listen"),
            Endpoint::loopback(80).expect("target"),
        );
        let mut book = ForwardBook::new();
        book.insert(rule.clone()).expect("insert");
        let mut center = ForwardCenter::new();
        center.replace_book(book);
        let start = center.prepare_start(rule.id(), None).expect("decision");
        assert!(matches!(start, ForwardStart::Exposure { .. }));
        assert!(!center.any_active());
        center.confirm_exposure(rule.id());
        let start = center
            .prepare_start(rule.id(), None)
            .expect("after confirm");
        assert!(matches!(start, ForwardStart::Connection { .. }));
        assert!(!center.any_active(), "no connection means no listener");
    }

    #[test]
    fn a_failure_keeps_the_saved_rule_and_is_not_listening() {
        let id_host = host();
        let rule = local(id_host, 4402);
        let mut book = ForwardBook::new();
        book.insert(rule.clone()).expect("insert");
        let mut center = ForwardCenter::new();
        center.replace_book(book.clone());
        center.phase.insert(
            rule.id(),
            ForwardPhase::Failed {
                generation: 1,
                failure: ForwardFailure::PortInUse {
                    host: "127.0.0.1".to_owned(),
                    port: 4402,
                },
            },
        );
        assert_eq!(center.book().rules().len(), 1);
        assert!(!center.any_active());
        assert!(matches!(
            center.rows()[0].phase,
            ForwardPhase::Failed {
                failure: ForwardFailure::PortInUse { port: 4402, .. },
                ..
            }
        ));
    }

    #[test]
    fn stopping_one_session_leaves_another_sessions_rule_alone() {
        let id_host = host();
        let first = local(id_host, 4403);
        let second = local(id_host, 4404);
        let mut book = ForwardBook::new();
        book.insert(first.clone()).expect("first");
        book.insert(second.clone()).expect("second");
        let mut center = ForwardCenter::new();
        center.replace_book(book);
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        center.phase.insert(
            first.id(),
            ForwardPhase::Listening {
                generation: 1,
                session: session_a,
                host: "127.0.0.1".to_owned(),
                port: 4403,
            },
        );
        center.phase.insert(
            second.id(),
            ForwardPhase::Listening {
                generation: 1,
                session: session_b,
                host: "127.0.0.1".to_owned(),
                port: 4404,
            },
        );
        let stopped = center.stop_session(session_a);
        assert_eq!(stopped, vec![first.id()]);
        assert!(matches!(
            center.phase.get(&first.id()),
            Some(ForwardPhase::Stopped)
        ));
        assert!(center.phase.get(&second.id()).unwrap().is_active());
        center.mark_carrier_lost(session_a);
        assert!(
            matches!(center.phase.get(&first.id()), Some(ForwardPhase::Stopped)),
            "a rule the user stopped stays stopped when the carrier then drops"
        );
        assert!(matches!(
            center.phase.get(&second.id()),
            Some(ForwardPhase::Listening { .. })
        ));
    }

    #[test]
    fn saving_a_different_shape_stops_the_listener_and_an_unchanged_save_does_not() {
        let id_host = host();
        let rule = ForwardRule::local(
            id_host,
            Endpoint::new("0.0.0.0", 4500).expect("listen"),
            Endpoint::loopback(80).expect("target"),
        );
        let mut book = ForwardBook::new();
        book.insert(rule.clone()).expect("insert");
        let mut center = ForwardCenter::new();
        center.replace_book(book.clone());
        let session = SessionId::new();
        center.phase.insert(
            rule.id(),
            ForwardPhase::Listening {
                generation: 4,
                session,
                host: "0.0.0.0".to_owned(),
                port: 4500,
            },
        );
        let (link, mut observed) = ForwardLink::observe();
        center.attach(session, link);

        center.adopt(book);
        assert!(
            matches!(
                center.phase.get(&rule.id()),
                Some(ForwardPhase::Listening {
                    port: 4500,
                    host,
                    ..
                }) if host == "0.0.0.0"
            ),
            "the same endpoints stay listening"
        );
        assert!(
            observed.commands().is_empty(),
            "an unchanged save sends nothing"
        );

        let narrowed = ForwardRule::rebuild(
            rule.id(),
            id_host,
            ForwardKind::Local,
            Endpoint::loopback(4500).expect("loopback"),
            Some(Endpoint::loopback(80).expect("target")),
        )
        .expect("rebuild");
        let mut narrowed_book = ForwardBook::new();
        narrowed_book.insert(narrowed).expect("insert");
        center.adopt(narrowed_book);
        assert!(
            matches!(center.phase.get(&rule.id()), Some(ForwardPhase::Stopped)),
            "the new loopback address is not the listener that was bound"
        );
        assert!(!center.any_active());
        assert_eq!(
            observed.commands(),
            vec![ForwardCommand::Stop { id: rule.id() }],
            "the old generation is stopped, so that channel cannot be used"
        );
        observed.push(ForwardEvent::Listening {
            id: rule.id(),
            generation: 4,
            host: "0.0.0.0".to_owned(),
            port: 4500,
        });
        assert!(center.pump());
        assert!(
            matches!(center.phase.get(&rule.id()), Some(ForwardPhase::Stopped)),
            "a late listening event does not restore the generation that was stopped"
        );

        let exposed = ForwardRule::rebuild(
            rule.id(),
            id_host,
            ForwardKind::Local,
            Endpoint::new("0.0.0.0", 4500).expect("listen"),
            Some(Endpoint::new("10.0.0.8", 81).expect("target")),
        )
        .expect("rebuild");
        let mut exposed_book = ForwardBook::new();
        exposed_book.insert(exposed).expect("insert");
        center.confirm_exposure(rule.id());
        center.adopt(exposed_book);
        assert!(matches!(
            center.phase.get(&rule.id()),
            Some(ForwardPhase::Stopped)
        ));
        let start = center
            .prepare_start(rule.id(), Some(session))
            .expect("decision");
        assert!(
            matches!(start, ForwardStart::Exposure { .. }),
            "a non-loopback listen asks again before the next real start"
        );
        assert!(!center.any_active());
        center.confirm_exposure(rule.id());
        let started = center
            .prepare_start(rule.id(), Some(session))
            .expect("after confirm");
        assert!(matches!(started, ForwardStart::Ready { .. }));
        let generation = match center.phase.get(&rule.id()) {
            Some(ForwardPhase::Starting { generation, .. }) => *generation,
            other => panic!("the new start is not listening yet: {other:?}"),
        };
        observed.push(ForwardEvent::Listening {
            id: rule.id(),
            generation,
            host: "0.0.0.0".to_owned(),
            port: 4500,
        });
        assert!(center.pump());
        assert!(
            center.is_listening_for_test(rule.id()),
            "a later start can listen again after the old one was stopped"
        );

        let retargeted = ForwardRule::rebuild(
            rule.id(),
            id_host,
            ForwardKind::Local,
            Endpoint::new("0.0.0.0", 4500).expect("listen"),
            Some(Endpoint::new("10.0.0.8", 82).expect("target")),
        )
        .expect("rebuild");
        let mut retargeted_book = ForwardBook::new();
        retargeted_book.insert(retargeted).expect("insert");
        center.adopt(retargeted_book);
        assert!(
            matches!(center.phase.get(&rule.id()), Some(ForwardPhase::Stopped)),
            "a new target is not the path the listener is still using"
        );

        let other_host = host();
        center.note_listening_for_test(rule.id(), session, "0.0.0.0", 4500);
        let moved = ForwardRule::rebuild(
            rule.id(),
            other_host,
            ForwardKind::Local,
            Endpoint::new("0.0.0.0", 4500).expect("listen"),
            Some(Endpoint::new("10.0.0.8", 82).expect("target")),
        )
        .expect("rebuild");
        let mut moved_book = ForwardBook::new();
        moved_book.insert(moved).expect("insert");
        center.adopt(moved_book);
        assert!(
            matches!(center.phase.get(&rule.id()), Some(ForwardPhase::Stopped)),
            "a different host is not the connection that is still carrying the rule"
        );

        center.note_listening_for_test(rule.id(), session, "0.0.0.0", 4500);
        let dynamic = ForwardRule::rebuild(
            rule.id(),
            other_host,
            ForwardKind::Dynamic,
            Endpoint::loopback(4500).expect("listen"),
            None,
        )
        .expect("rebuild");
        let mut dynamic_book = ForwardBook::new();
        dynamic_book.insert(dynamic).expect("insert");
        center.adopt(dynamic_book);
        assert!(
            matches!(center.phase.get(&rule.id()), Some(ForwardPhase::Stopped)),
            "a different kind is not the listener that is still bound"
        );
        let stops = observed
            .commands()
            .into_iter()
            .filter(|command| matches!(command, ForwardCommand::Stop { id } if *id == rule.id()))
            .count();
        assert!(
            stops >= 3,
            "each shape change while the rule is active stops that generation, got {stops}"
        );
    }
}
