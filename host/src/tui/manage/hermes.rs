//! The Hermes connections sheet: the gateways on the user's other computers (their VPS), where cloud bots run.
//! The TUI runs on the computer, so it adds, edits, tests and removes them as the desktop app does; the key for
//! each profile is typed here and kept in this computer's vault.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::borrow::Cow;

use super::super::app::{App, Editor, Overlay, cycle};
use super::Reply;

/// The fields of a gateway being added or edited, in order.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum GatewayField {
    #[default]
    Name,
    Address,
    Profile,
    Key,
    Ssh,
}

impl GatewayField {
    pub const ALL: [Self; 5] = [Self::Name, Self::Address, Self::Profile, Self::Key, Self::Ssh];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Address => "Address",
            Self::Profile => "Profile",
            Self::Key => "Key",
            Self::Ssh => "SSH host",
        }
    }

    fn next(self, by: isize) -> Self {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[cycle(i, by, Self::ALL.len())]
    }
}

/// A gateway as the form holds it: what the user typed, and what `testConnection` said.
#[derive(Default)]
pub struct GatewayForm {
    /// The gateway being edited; `None` adds one.
    pub id: Option<String>,
    pub name: Editor,
    pub address: Editor,
    pub profile: Editor,
    /// The key for that profile: kept as typed (never shown on screen), empty keeps the saved one.
    pub key: Editor,
    pub ssh: Editor,
    pub field: GatewayField,
    /// `testConnection` is running.
    pub testing: bool,
    /// What it found: the models of the profile it checked.
    pub tested: Option<Vec<String>>,
    pub error: Option<String>,
    pub saving: bool,
}

impl GatewayForm {
    /// A new gateway: the form as the desktop app opens it.
    fn new() -> Self {
        Self::default()
    }

    /// An existing one, with its saved values (the key stays on the computer, so it starts empty).
    fn edit(c: &Value) -> Self {
        Self {
            id: c["id"].as_str().map(str::to_owned),
            name: Editor::with(c["name"].as_str().unwrap_or_default()),
            address: Editor::with(c["baseUrl"].as_str().unwrap_or_default()),
            profile: Editor::with(c["defaultProfile"].as_str().unwrap_or("default")),
            key: Editor::default(),
            ssh: Editor::with(c["ssh"].as_str().unwrap_or_default()),
            field: GatewayField::Name,
            testing: false,
            tested: None,
            error: None,
            saving: false,
        }
    }

    /// The profile this gateway's plain routes serve.
    fn profile(&self) -> Cow<'_, str> {
        match self.profile.text.trim() {
            "" => Cow::Borrowed("default"),
            name => Cow::Owned(name.to_ascii_lowercase()),
        }
    }

    /// `saveConnection`'s body: the key goes only when one was typed.
    fn save_body(&self) -> Value {
        let mut body = json!({
            "name": self.name.text.trim(),
            "baseUrl": self.address.text.trim(),
            "profile": self.profile(),
            "ssh": self.ssh.text.trim(),
        });
        if let Some(id) = &self.id {
            body["id"] = id.clone().into();
        }
        if !self.key.text.trim().is_empty() {
            body["key"] = self.key.text.as_str().into();
        }
        body
    }

    /// `testConnection`'s body: the saved gateway by id, or what's typed (whose key is used for this test alone).
    fn test_body(&self) -> Value {
        match &self.id {
            Some(id) => json!({"id": id, "profile": self.profile()}),
            None => json!({
                "baseUrl": self.address.text.trim(),
                "profile": self.profile(),
                "key": self.key.text.trim(),
            }),
        }
    }

    /// Whether there's enough to test: an address, and (for one not saved yet) its key.
    fn testable(&self) -> bool {
        !self.address.text.trim().is_empty() && (self.id.is_some() || !self.key.text.trim().is_empty())
    }

    fn savable(&self) -> bool {
        !self.name.text.trim().is_empty() && !self.address.text.trim().is_empty()
    }

    fn editor(&mut self) -> &mut Editor {
        match self.field {
            GatewayField::Name => &mut self.name,
            GatewayField::Address => &mut self.address,
            GatewayField::Profile => &mut self.profile,
            GatewayField::Key => &mut self.key,
            GatewayField::Ssh => &mut self.ssh,
        }
    }
}

/// The connections sheet: the list, or a gateway's form.
#[derive(Default)]
pub struct HermesSheet {
    /// `listConnections`'s answer; `None` until it comes.
    pub connections: Option<Vec<Value>>,
    pub cursor: usize,
    pub step: Step,
    pub error: Option<String>,
    pub busy: bool,
}

#[derive(Default)]
pub enum Step {
    #[default]
    List,
    Form(Box<GatewayForm>),
    /// Removing the gateway `name` (`id`).
    Remove {
        id: String,
        name: String,
    },
}

impl HermesSheet {
    pub fn list(&self) -> &[Value] {
        self.connections.as_deref().unwrap_or_default()
    }

    fn selected(&self) -> Option<&Value> {
        self.list().get(self.cursor)
    }

    /// Takes a new list, keeping the cursor on a gateway that's still there.
    fn show(&mut self, connections: Vec<Value>) {
        self.connections = Some(connections);
        self.cursor = self.cursor.min(self.list().len().saturating_sub(1));
    }

    /// Bots that run on gateway `id`, by name.
    pub fn bots<'a>(app: &'a App, id: &str) -> Vec<&'a super::super::app::Bot> {
        let mut bots: Vec<&super::super::app::Bot> =
            app.bots.values().filter(|b| b.connection.as_deref() == Some(id)).collect();
        bots.sort_by(|a, b| a.name.cmp(&b.name));
        bots
    }
}

/// The profile a bot runs as on either side (`remote::hermes::profile_of`): its own name, or one from its id.
pub fn profile_of(id: &str, named: Option<&str>) -> String {
    match named.map(str::trim).filter(|p| !p.is_empty()) {
        Some(profile) => profile.to_ascii_lowercase(),
        None => format!("codync-{}", id.chars().filter(char::is_ascii_hexdigit).take(12).collect::<String>()),
    }
}

/// The list a `hello` or a `connections` event carries, into the sheet if it's open.
pub(in crate::tui) fn loaded(overlays: &mut [Overlay], connections: &[Value]) {
    for o in overlays {
        if let Overlay::Hermes(h) = o {
            h.show(connections.to_vec());
        }
    }
}

impl App {
    pub fn open_hermes(&mut self) {
        self.sheet("listConnections", json!({}), Reply::Connections);
        self.overlays.push(Overlay::Hermes(Box::default()));
    }

    pub(in crate::tui) fn hermes_reply(&mut self, r: &Reply, res: Result<Value, String>) {
        let mut flash: Option<String> = None;
        let mut reload = false;
        {
            let Some(Overlay::Hermes(h)) = self.overlays.iter_mut().rev().find(|o| matches!(o, Overlay::Hermes(_)))
            else {
                return;
            };
            h.busy = false;
            let v = match res {
                Ok(v) => v,
                Err(e) => {
                    match r {
                        Reply::GatewaySaved => {
                            if let Step::Form(f) = &mut h.step {
                                f.saving = false;
                                f.error = Some(e);
                            }
                        }
                        Reply::GatewayTested => {
                            if let Step::Form(f) = &mut h.step {
                                f.testing = false;
                                f.error = Some(e);
                            }
                        }
                        _ => h.error = Some(e),
                    }
                    return;
                }
            };
            h.error = None;
            match r {
                Reply::Connections => h.show(v["connections"].as_array().cloned().unwrap_or_default()),
                Reply::GatewaySaved => {
                    h.step = Step::List;
                    flash = Some("Gateway saved".into());
                    reload = true;
                }
                Reply::GatewayTested => {
                    if let Step::Form(f) = &mut h.step {
                        f.testing = false;
                        f.tested = Some(
                            v["models"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|m| m.as_str().map(str::to_owned))
                                .collect(),
                        );
                        flash = Some(format!("{} answered", v["profile"].as_str().unwrap_or_default()));
                    }
                }
                Reply::GatewayRemoved => {
                    h.step = Step::List;
                    flash = Some("Gateway removed".into());
                    reload = true;
                }
                _ => {}
            }
        }
        if let Some(text) = flash {
            self.flash(&text);
        }
        if reload {
            self.sheet("listConnections", json!({}), Reply::Connections);
        }
    }

    pub(in crate::tui) fn hermes_key(&mut self, mut h: Box<HermesSheet>, k: KeyEvent) -> Option<Overlay> {
        let step = std::mem::take(&mut h.step);
        h.step = match step {
            Step::List => return Self::hermes_list_key(h, k),
            Step::Form(f) => self.gateway_form_key(f, k),
            Step::Remove { id, name } => self.gateway_remove_key(&mut h, id, name, k),
        };
        Some(Overlay::Hermes(h))
    }

    fn hermes_list_key(mut h: Box<HermesSheet>, k: KeyEvent) -> Option<Overlay> {
        let last = h.list().len().saturating_sub(1);
        let selected = h.selected().cloned();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Down | KeyCode::Char('j') => h.cursor = (h.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => h.cursor = h.cursor.saturating_sub(1),
            KeyCode::Char('n') if h.connections.is_some() => {
                h.error = None;
                h.step = Step::Form(Box::new(GatewayForm::new()));
            }
            KeyCode::Enter if selected.is_some() => {
                if let Some(c) = selected {
                    h.step = Step::Form(Box::new(GatewayForm::edit(&c)));
                }
            }
            KeyCode::Char('x') if selected.is_some() => {
                if let Some(c) = selected {
                    h.step = Step::Remove {
                        id: c["id"].as_str().unwrap_or_default().to_owned(),
                        name: c["name"].as_str().unwrap_or_default().to_owned(),
                    };
                }
            }
            _ => {}
        }
        Some(Overlay::Hermes(h))
    }

    fn gateway_form_key(&mut self, mut f: Box<GatewayForm>, k: KeyEvent) -> Step {
        if f.saving {
            return if k.code == KeyCode::Esc { Step::List } else { Step::Form(f) };
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return Step::List,
            KeyCode::Tab | KeyCode::Down => f.field = f.field.next(1),
            KeyCode::BackTab | KeyCode::Up => f.field = f.field.next(-1),
            KeyCode::Char('s') if ctrl => {
                if f.savable() {
                    f.saving = true;
                    f.error = None;
                    let body = f.save_body();
                    self.sheet("saveConnection", body, Reply::GatewaySaved);
                } else {
                    f.error = Some("A name and an address are required.".into());
                }
            }
            KeyCode::Char('t') if ctrl => {
                if f.testable() {
                    f.testing = true;
                    f.tested = None;
                    f.error = None;
                    let body = f.test_body();
                    self.sheet("testConnection", body, Reply::GatewayTested);
                } else {
                    f.error = Some("Enter its address and key first.".into());
                }
            }
            _ => {
                f.editor().key(k);
            }
        }
        Step::Form(f)
    }

    fn gateway_remove_key(&mut self, h: &mut HermesSheet, id: String, name: String, k: KeyEvent) -> Step {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => Step::List,
            KeyCode::Enter if !h.busy => {
                h.busy = true;
                self.sheet("deleteConnection", json!({"id": id}), Reply::GatewayRemoved);
                Step::Remove { id, name }
            }
            _ => Step::Remove { id, name },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::app::test_app;
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn a_profile_defaults_and_goes_lowercase() {
        let mut f = GatewayForm::new();
        assert_eq!(f.profile(), "default");
        f.profile = Editor::with(" Codync-A ");
        assert_eq!(f.profile(), "codync-a");
        assert_eq!(f.test_body()["profile"], "codync-a");
    }

    #[test]
    fn a_saved_gateway_tests_by_id_and_a_new_one_by_its_typed_key() {
        let mut f = GatewayForm::new();
        f.address = Editor::with("https://vps.example.com");
        assert!(!f.testable(), "a new one needs its key");
        f.key = Editor::with(&"k".repeat(16));
        assert!(f.testable());
        assert_eq!(f.test_body()["baseUrl"], "https://vps.example.com");
        f.id = Some("c".into());
        assert_eq!(f.test_body()["id"], "c");
        assert!(f.save_body().get("key").is_some(), "the typed key is saved");
        f.key.clear();
        assert!(f.save_body().get("key").is_none(), "leaving it out keeps the saved one");
    }

    #[test]
    fn profiles_follow_the_hosts_naming() {
        assert_eq!(profile_of("b0f1c2d3-4e5f-6789-abcd-ef0123456789", None), "codync-b0f1c2d34e5f");
        assert_eq!(profile_of("b", Some("Named")), "named");
    }

    #[tokio::test]
    async fn the_list_opens_forms_and_confirms_removal() {
        let mut app = test_app();
        app.open_hermes();
        let list = [json!({"id": "c", "name": "VPS", "baseUrl": "https://vps.example.com"})];
        loaded(&mut app.overlays, &list);
        let Some(Overlay::Hermes(h)) = app.overlays.pop() else { panic!("the sheet is open") };
        let Some(Overlay::Hermes(h)) = app.hermes_key(h, key(KeyCode::Char('n'))) else { panic!("stays open") };
        assert!(matches!(h.step, Step::Form(_)), "n adds one");
        let Some(Overlay::Hermes(h)) = app.hermes_key(h, key(KeyCode::Esc)) else { panic!("stays open") };
        let Some(Overlay::Hermes(h)) = app.hermes_key(h, key(KeyCode::Enter)) else { panic!("stays open") };
        match &h.step {
            Step::Form(f) => assert_eq!(f.name.text, "VPS"),
            _ => panic!("enter edits the shown gateway"),
        }
        let Some(Overlay::Hermes(h)) = app.hermes_key(h, key(KeyCode::Esc)) else { panic!("stays open") };
        let Some(Overlay::Hermes(h)) = app.hermes_key(h, key(KeyCode::Char('x'))) else { panic!("stays open") };
        assert!(matches!(h.step, Step::Remove { .. }));
    }
}
