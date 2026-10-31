//! The Hermes connections sheet: the gateways on the user's other computers, and one's form.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::super::app::App;
use super::super::l10n::tr;
use super::super::manage::hermes::{GatewayField, GatewayForm, HermesSheet, Step, profile_of};
use super::super::md;
use super::buffer::{centered, frame_box, hline, put, restyle, rput, u};
use super::network::wrapped;
use super::overlay::input_line;
use super::style::theme;
use super::time::{MONTHS, local, month_day};

pub(super) fn hermes(buf: &mut Buffer, area: Rect, app: &App, h: &HermesSheet) {
    let t = theme();
    let r = centered(area, 88, area.height.saturating_sub(4).min(26));
    let inner = frame_box(buf, r, t.text, t.panel, Some((tr("Hermes connections").as_ref(), t.text)));
    if inner.height < 8 {
        return;
    }
    hline(buf, inner.x, inner.bottom() - 2, inner.width, t.dim.patch(t.panel));
    let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 2);
    let keys = match &h.step {
        Step::List => list(buf, body, app, h),
        Step::Form(f) => form(buf, body, f),
        Step::Remove { name, .. } => remove(buf, body, name),
    };
    if let Some(e) = &h.error {
        put(buf, inner.x, inner.bottom() - 3, inner.width, e, t.red.patch(t.panel));
    } else if h.busy {
        put(buf, inner.x, inner.bottom() - 3, inner.width, &tr("Working…"), t.dim.patch(t.panel));
    }
    put(buf, inner.x, inner.bottom() - 1, inner.width, &format!(" {}", tr(keys)), t.dim.patch(t.panel));
}

fn list(buf: &mut Buffer, body: Rect, app: &App, h: &HermesSheet) -> &'static str {
    let t = theme();
    let mut y = body.y;
    if h.connections.is_none() {
        put(buf, body.x, y, body.width, &tr("Loading…"), t.dim.patch(t.panel));
        return "esc close";
    }
    if h.list().is_empty() {
        put(buf, body.x, y, body.width, &tr("No gateways yet."), t.bold.patch(t.panel));
        y += 1;
        let note = tr(
            "A gateway is a Hermes install on a computer you keep running (your VPS). Bots on it work here beside this computer's bots, in the same chats.",
        );
        wrapped(buf, body, &mut y, &note, t.secondary);
        y += 1;
        wrapped(buf, body, &mut y, &tr("n adds one: its address, and the key for the profile it serves."), t.dim);
        return "n new gateway · esc close";
    }
    let room = usize::from(body.height.saturating_sub(3));
    let off = h.cursor.saturating_sub(room.saturating_sub(1));
    for (i, c) in h.list().iter().enumerate().skip(off) {
        if y >= body.bottom().saturating_sub(1) {
            break;
        }
        let sel = i == h.cursor;
        let base = if sel { t.sel } else { t.panel };
        if sel {
            restyle(buf, Rect::new(body.x - 1, y, body.width + 2, 1), t.sel);
        }
        let name = c["name"].as_str().unwrap_or_default();
        let address = c["baseUrl"].as_str().unwrap_or_default();
        put(buf, body.x, y, 2, if sel { "▸" } else { " " }, t.secondary.patch(base));
        put(buf, body.x + 2, y, 20, name, t.bold.patch(base));
        let profile = c["defaultProfile"].as_str().unwrap_or("default");
        let reach = c["verifiedAt"].as_i64().map_or_else(
            || tr("Not checked").into_owned(),
            |at| {
                let (month, day) = month_day(local(at).0);
                format!("{} {}", MONTHS[month], day)
            },
        );
        let right = format!("{profile} · {reach}");
        let rx = rput(buf, body.right(), y, &right, t.dim.patch(base));
        let room = rx.saturating_sub(body.x + 24);
        put(buf, body.x + 22, y, room, address, t.secondary.patch(base));
        y += 1;
        // Its bots and the profile each runs as, one line under it.
        let id = c["id"].as_str().unwrap_or_default();
        let bots = HermesSheet::bots(app, id);
        let text = if bots.is_empty() {
            tr("No bots run on it yet.").into_owned()
        } else {
            bots.iter()
                .map(|b| format!("{} ({})", b.name, profile_of(&b.id, b.hermes_profile.as_deref())))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let line = format!("    {text}");
        put(buf, body.x, y, body.width, &md::truncate(&line, usize::from(body.width)), t.dim.patch(t.panel));
        y += 2;
    }
    if h.list().len() > room {
        rput(buf, body.right(), body.bottom() - 1, &format!("{} gateways", h.list().len()), t.dim.patch(t.panel));
    }
    "↵ edit · n new · x remove · esc close"
}

fn form(buf: &mut Buffer, body: Rect, f: &GatewayForm) -> &'static str {
    let t = theme();
    let mut y = body.y;
    let title = if f.id.is_some() { "Edit gateway" } else { "Add a gateway" };
    put(buf, body.x, y, body.width, &tr(title), t.bold.patch(t.panel));
    y += 2;
    let label_w = 12u16;
    for field in GatewayField::ALL {
        if y >= body.bottom().saturating_sub(1) {
            break;
        }
        let active = field == f.field;
        let style = if active { t.bold } else { t.secondary };
        let label = tr(field.label());
        put(buf, body.x, y, label_w, &label, style.patch(t.panel));
        let r = Rect::new(body.x + label_w, y, body.width.saturating_sub(label_w), 1);
        if active {
            restyle(buf, Rect::new(body.x + label_w - 1, y, r.width + 1, 1), t.sel);
        }
        let (editor, placeholder, secret) = match field {
            GatewayField::Name => (&f.name, "VPS", false),
            GatewayField::Address => (&f.address, "https://vps.example.com", false),
            GatewayField::Profile => (&f.profile, "default", false),
            GatewayField::Key => (&f.key, "Its key for this profile (kept on this computer)", true),
            GatewayField::Ssh => (&f.ssh, "user@host, to make profiles there (optional)", false),
        };
        // A key is typed, never shown back.
        if secret && !editor.text.is_empty() {
            let shown = "•".repeat(md::width(&editor.text).min(usize::from(r.width)));
            put(buf, r.x, y, r.width, &shown, t.text.patch(t.sel));
            if active {
                super::set_cursor(ratatui::layout::Position::new(r.x + u(md::width(&editor.text)), y));
            }
        } else {
            input_line(buf, r, y, editor, placeholder, active, false);
        }
        y += 1;
    }
    y += 1;
    // What the test found.
    if f.testing {
        put(buf, body.x, y, body.width, &tr("Testing…"), t.dim.patch(t.panel));
    } else if let Some(models) = &f.tested {
        let text = if models.is_empty() {
            tr("It answered, with no models listed.").into_owned()
        } else {
            format!("{} · {}", tr("Models"), models.join(", "))
        };
        let mut yy = y;
        wrapped(buf, body, &mut yy, &text, t.green);
    } else {
        let note = tr("t tests it: the gateway's own capabilities and the models of the profile above.");
        wrapped(buf, body, &mut y, &note, t.dim);
    }
    "ctrl+s save · ctrl+t test · tab next field · esc back"
}

fn remove(buf: &mut Buffer, body: Rect, name: &str) -> &'static str {
    let t = theme();
    let mut y = body.y;
    line(buf, body, &mut y, &tr("Remove {name}?").replace("{name}", name), t.red);
    let note = tr("Its bots must move to this computer or another gateway first; the keys it held go with it.");
    wrapped(buf, body, &mut y, &note, t.text);
    "↵ remove · esc cancel"
}

fn line(buf: &mut Buffer, body: Rect, y: &mut u16, text: &str, style: Style) {
    if *y < body.bottom() {
        put(buf, body.x, *y, body.width, text, style.patch(theme().panel));
        *y += 1;
    }
}
