//! Popcorn, a security workbench: disposable, isolated microVMs for risky work with Claude Code.
mod gate;
mod icons;
mod sandbox;
mod share;
mod theme;

use gate::{Answer, Event, Kind};
use gpui::{prelude::*, *};
use sandbox::{Case, Mode};
use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;
use theme::{Appearance, CODE_FONT, UI_FONT, WINDOW_RADIUS, theme};

/// Invisible band around the window (Linux client-side decorations) that holds the shadow and resize handles.
const INSET: Pixels = px(10.);
const PANEL_RADIUS: Pixels = px(10.);

#[derive(Clone, PartialEq)]
enum Build {
    Idle,
    Running(String),
    Failed(String),
}

struct Review {
    corn: usize,
    changes: Vec<share::Change>,
    open: Option<(usize, String)>,
}

struct Popcorn {
    cases: Vec<Case>,
    selected: usize,
    build: Build,
    has_key: bool,
    /// The token being typed or pasted on the setup screen. Never shown in full.
    token: String,
    token_focus: FocusHandle,
    /// What cases call you, as typed on the setup screen.
    name: String,
    name_focus: FocusHandle,
    has_name: bool,
    /// You chose to set up Claude later.
    claude_skipped: bool,
    /// The Connect Claude dialog is open.
    connecting: bool,
    /// The setup command was just copied; the button says so.
    copied: bool,
    focus: FocusHandle,
    /// Last error or confirmation, shown in context.
    notice: Option<(String, bool)>,
    /// The case the delete dialog is asking about.
    deleting: Option<usize>,
    /// The intro page was dismissed this session.
    intro_seen: bool,
    /// Changes pulled back from a corn's shared folder, waiting for your decision.
    review: Option<Review>,
    /// The tab being renamed, with the text typed so far.
    renaming: Option<(usize, String)>,
    rename_focus: FocusHandle,
    should_move: bool,
    appearance: Appearance,
    _appearance_watch: Subscription,
}

fn ui_icon(name: &str) -> Svg {
    svg().path(format!("icons/ui/{name}.svg")).flex_shrink_0()
}

/// Window button for Linux and Windows. Stops mouse-down so the title bar never starts a drag
/// from it: on Wayland a drag hands the pointer to the compositor and the click would be lost.
fn control(id: &'static str, icon: &'static str, area: WindowControlArea, danger: bool, action: fn(&mut Window)) -> impl IntoElement {
    let t = theme();
    div()
        .id(id)
        .group(id)
        .w(px(40.))
        .h(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .window_control_area(area)
        .hover(move |s| s.bg(if danger { t.bad } else { t.selected }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            action(window);
        })
        .child(
            ui_icon(icon)
                .size(px(16.))
                .text_color(t.muted)
                .group_hover(id, move |s| s.text_color(if danger { rgb(0xffffff).into() } else { t.fg })),
        )
}

/// Title bar icon button that is not a window control.
fn control_like(id: &'static str, icon: &'static str, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    let t = theme();
    div()
        .id(id)
        .group(id)
        .w(px(36.))
        .h(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .hover(|s| s.bg(t.selected))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |e, window, cx| {
            cx.stop_propagation();
            on_click(e, window, cx);
        })
        .child(ui_icon(icon).size(px(16.)).text_color(t.muted).group_hover(id, |s| s.text_color(t.fg)))
}

fn resize_edge(pos: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
    let corner = INSET * 2.;
    let (top, bottom) = (pos.y < INSET, pos.y > size.height - INSET);
    let (left, right) = (pos.x < INSET, pos.x > size.width - INSET);
    let (near_top, near_bottom) = (pos.y < corner, pos.y > size.height - corner);
    let (near_left, near_right) = (pos.x < corner, pos.x > size.width - corner);
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeEdge::TopLeft,
        _ if (top && near_right) || (right && near_top) => ResizeEdge::TopRight,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeEdge::BottomLeft,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeEdge::BottomRight,
        _ if top => ResizeEdge::Top,
        _ if bottom => ResizeEdge::Bottom,
        _ if left => ResizeEdge::Left,
        _ if right => ResizeEdge::Right,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// Graphite fill: the one thing to do next.
    Primary,
    /// Hairline outline.
    Plain,
    /// Text only.
    Quiet,
    /// Oxide fill: erases something.
    Danger,
}

fn button(id: impl Into<ElementId>, icon: Option<&'static str>, label: &str, look: Look) -> Stateful<Div> {
    let t = theme();
    let (bg, fg) = match look {
        Look::Primary => (t.fg, t.bg),
        Look::Danger => (t.bad, rgb(0xffffff).into()),
        _ => (gpui::transparent_black(), t.fg),
    };
    div()
        .id(id.into())
        .h(px(34.))
        .px(px(if look == Look::Quiet { 8. } else { 14. }))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(13.5))
        .font_weight(FontWeight::MEDIUM)
        .bg(bg)
        .text_color(fg)
        .when(look == Look::Plain, |d| d.border_1().border_color(t.seam))
        .when(matches!(look, Look::Primary | Look::Danger), |d| d.hover(|s| s.opacity(0.88)))
        .when(matches!(look, Look::Plain | Look::Quiet), |d| d.hover(|s| s.bg(t.hover)))
        .when_some(icon, |d, i| d.child(ui_icon(i).size(px(15.)).text_color(fg)))
        .child(label.to_string())
}

/// A key cap, for the shortcuts shown next to decisions.
fn key(k: &str, on: Hsla, ink: Hsla) -> Div {
    div()
        .min_w(px(20.))
        .h(px(20.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(ink.opacity(0.35))
        .bg(on)
        .text_size(px(11.))
        .font_family(CODE_FONT)
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .child(k.to_string())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(at: u64) -> String {
    match now().saturating_sub(at) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

fn running_for(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => "Running, started just now".into(),
        s if s < 3600 => format!("Running for {} min", s / 60),
        s => format!("Running for {} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// Diagonal hazard stripes, painted across the given bounds.
fn stripes(ink: Hsla) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let (w, h) = (b.size.width, b.size.height);
            let step = px(14.);
            let mut x = -h;
            while x < w {
                let mut p = PathBuilder::fill();
                p.move_to(b.origin + point(x, h));
                p.line_to(b.origin + point(x + h, px(0.)));
                p.line_to(b.origin + point(x + h + step / 2., px(0.)));
                p.line_to(b.origin + point(x + step / 2., h));
                p.close();
                if let Ok(p) = p.build() {
                    window.paint_path(p, ink);
                }
                x += step;
            }
        },
    )
}

/// Counts for the airlock, read from the case's activity.
#[derive(Default)]
struct Crossings {
    claude: usize,
    allowed: usize,
    refused: usize,
    files_in: usize,
    files_out: usize,
}

fn crossings(events: &[Event]) -> Crossings {
    let mut c = Crossings::default();
    for e in events {
        match e.kind {
            Kind::Claude => c.claude += 1,
            Kind::Blocked => c.refused += 1,
            Kind::Allowed => c.allowed += 1,
            Kind::Note if e.text.starts_with("Copied ") => c.files_in += 1,
            Kind::Note if e.text.starts_with("Exported ") => {
                c.files_out += e.text.split_whitespace().nth(1).and_then(|n| n.parse::<usize>().ok()).unwrap_or(0)
            }
            Kind::Note => {}
        }
    }
    c
}

impl Popcorn {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Repaint regularly so VM state, questions and the activity stay live.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut me = Self {
            cases: Case::list(),
            selected: 0,
            build: Build::Idle,
            has_key: gate::credential().is_some(),
            token: String::new(),
            token_focus: cx.focus_handle(),
            name: String::new(),
            name_focus: cx.focus_handle(),
            has_name: sandbox::user_name().is_some(),
            claude_skipped: std::env::home_dir().is_some_and(|h| h.join(".config/popcorn/claude-skipped").exists()),
            connecting: false,
            copied: false,
            focus: cx.focus_handle(),
            notice: None,
            deleting: None,
            intro_seen: false,
            review: None,
            renaming: None,
            rename_focus: cx.focus_handle(),
            should_move: false,
            appearance: Appearance::load(),
            _appearance_watch: cx.observe_window_appearance(window, |this, window, cx| {
                this.apply_appearance(window);
                cx.notify();
            }),
        };
        me.apply_appearance(window);
        me
    }

    fn apply_appearance(&mut self, window: &Window) {
        theme::set_dark(self.appearance.is_dark(window.appearance()));
    }

    fn case(&mut self) -> Option<&mut Case> {
        self.cases.get_mut(self.selected)
    }

    fn report(&mut self, r: Result<(), String>) {
        self.notice = r.err().map(|e| (e, false));
    }

    fn new_case(&mut self) {
        match Case::create() {
            Ok(c) => {
                // New tabs open at the end, next to the + that made them.
                self.cases.push(c);
                self.selected = self.cases.len() - 1;
                self.notice = None;
            }
            Err(e) => self.notice = Some((e, false)),
        }
    }

    /// Answer the oldest question of the selected case.
    fn answer_first(&mut self, a: Answer) -> bool {
        let Some(c) = self.cases.get(self.selected) else { return false };
        let mut asks = c.policy.asks.lock().unwrap();
        if asks.is_empty() {
            return false;
        }
        asks.remove(0).answer(a);
        true
    }

    fn on_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        if self.deleting.is_some() {
            match k.key.as_str() {
                "escape" => self.deleting = None,
                "enter" => self.delete_confirmed(),
                _ => return,
            }
            cx.notify();
            return;
        }
        if k.modifiers.secondary() || k.modifiers.alt {
            return;
        }
        let a = match k.key.as_str() {
            "d" => Answer::Deny,
            "o" => Answer::Once,
            "a" => Answer::Always,
            _ => return,
        };
        if self.answer_first(a) {
            cx.notify();
        }
    }

    fn delete_confirmed(&mut self) {
        if let Some(i) = self.deleting.take().filter(|&i| i < self.cases.len()) {
            self.cases.remove(i).delete();
            if self.selected >= i && self.selected > 0 {
                self.selected -= 1;
            }
            self.notice = None;
        }
    }

    /// Asks before a case is erased: the machine, its disk, its activity and its exports.
    fn delete_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme();
        let i = self.deleting?;
        let c = self.cases.get(i)?;
        let name = c.name.clone();
        let exported = c.dir.join("exported").exists();
        Some(
            div()
                .id("delete-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.32))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.deleting = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(440.))
                        .p_6()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .bg(t.bg)
                        .rounded(px(10.))
                        .border_1()
                        .border_color(t.seam)
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).child(format!("Delete {name}?")))
                        .child(div().text_color(t.muted).line_height(px(21.)).child(if exported {
                            "This stops the machine and erases its disk, its activity and the files you exported from it. Copies you made elsewhere are not touched."
                        } else {
                            "This stops the machine and erases its disk and activity. Nothing on your computer is touched, including a shared folder."
                        }))
                        .child(
                            div()
                                .pt_3()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(button("cancel-delete", None, "Cancel", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.deleting = None;
                                    cx.notify();
                                })))
                                .child(button("confirm-delete", Some("trash-2"), "Delete corn", Look::Danger).on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_confirmed();
                                    cx.notify();
                                }))),
                        ),
                )
                .into_any_element(),
        )
    }

    fn on_rename_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let k = &e.keystroke;
        if matches!(k.key.as_str(), "enter" | "escape") {
            self.finish_rename(k.key == "enter", window);
            cx.notify();
            return;
        }
        let Some((_, text)) = self.renaming.as_mut() else { return };
        match k.key.as_str() {
            "backspace" => {
                text.pop();
            }
            _ if k.modifiers.secondary() && k.key == "v" => {
                if let Some(p) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    text.push_str(p.lines().next().unwrap_or(""));
                }
            }
            _ => {
                if let Some(c) = k.key_char.as_deref().filter(|c| !k.modifiers.secondary() && !c.chars().any(char::is_control)) {
                    text.push_str(c);
                }
            }
        }
        // Names are saved at 48 characters at most; stop there while typing too.
        if let Some((_, text)) = self.renaming.as_mut() {
            *text = text.chars().take(48).collect();
        }
        cx.notify();
    }

    /// Close the rename box, keeping the new name when `save`.
    fn finish_rename(&mut self, save: bool, window: &mut Window) {
        if let Some((i, text)) = self.renaming.take() {
            if save {
                if let Some(c) = self.cases.get_mut(i) {
                    c.rename(&text);
                }
            }
        }
        window.focus(&self.focus);
    }

    fn on_name_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        cx.stop_propagation();
        if k.modifiers.secondary() && k.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                self.name.push_str(text.lines().next().unwrap_or("").trim());
            }
        } else if k.key == "backspace" {
            self.name.pop();
        } else if k.key == "enter" {
            self.save_name(window);
        } else if let Some(c) = k.key_char.as_deref().filter(|c| !k.modifiers.secondary() && !c.chars().any(char::is_control)) {
            self.name.push_str(c);
        }
        self.name = self.name.chars().take(40).collect();
        self.notice = None;
        cx.notify();
    }

    fn save_name(&mut self, window: &mut Window) {
        match sandbox::save_user_name(&self.name) {
            Ok(_) => {
                self.has_name = true;
                self.notice = None;
                window.focus(&self.token_focus);
            }
            Err(e) => self.notice = Some((e, false)),
        }
    }

    /// First thing a new person sees: what Popcorn and a corn are, then on to setup.
    fn intro(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let block = |icon: AnyElement, title: &str, body: &str| {
            div()
                .flex()
                .items_start()
                .gap_6()
                .child(div().w(px(76.)).flex_shrink_0().flex().justify_center().child(icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                        .child(div().text_size(px(15.)).text_color(t.muted).line_height(px(24.)).child(body.to_string())),
                )
        };
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(600.))
                    .flex()
                    .flex_col()
                    .gap_10()
                    .child(block(
                        div().text_size(px(62.)).line_height(px(72.)).child("🍿").into_any_element(),
                        "Popcorn",
                        "A workbench for risky work: untrusted code, suspicious packages, binaries you would never run on your own machine. You and Claude do the work inside a corn, and your computer stays out of reach.",
                    ))
                    .child(block(
                        div().text_size(px(62.)).line_height(px(72.)).child("🌽").into_any_element(),
                        "Corn",
                        "A small, disposable virtual machine with its own Linux kernel. It cannot see your files, SSH keys or network. Every connection waits for your answer, your Claude token is added outside it, and files come back only when you approve them. Done? Delete it.",
                    ))
                    .child(div().pl(px(100.)).flex().child(button("lets-go", None, "Let's go!", Look::Primary).h(px(42.)).px_6().text_size(px(15.)).on_click(cx.listener(|this, _, window, cx| {
                        this.intro_seen = true;
                        window.focus(if this.has_name { &this.token_focus } else { &this.name_focus });
                        cx.notify();
                    })))),
            )
            .into_any_element()
    }

    /// The Claude token field with Save, the command that makes a token, and (on setup) Skip.
    fn token_form(&self, skip: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = theme();
        let focused = self.token_focus.is_focused(window);
        let shown = match self.token.len() {
            0 => None,
            n if n <= 12 => Some(self.token.clone()),
            n => Some(format!("{}{}", &self.token[..12], "•".repeat((n - 12).min(24)))),
        };
        let field = div()
            .id("token")
            .track_focus(&self.token_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.on_token_key(e, cx)))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.token_focus);
                cx.notify();
            }))
            .flex_1()
            .min_w_0()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(t.bg)
            .border_1()
            .border_color(if focused { t.fg } else { t.seam })
            .cursor(CursorStyle::IBeam)
            .font_family(CODE_FONT)
            .text_size(px(13.5))
            .child(match shown {
                Some(s) => div().truncate().child(s).into_any_element(),
                None if !focused => div().text_color(t.faint).child("sk-ant-oat01-…").into_any_element(),
                None => div().into_any_element(),
            })
            .when(focused, |d| d.child(div().w(px(1.5)).h(px(18.)).bg(t.fg)));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().gap_2().child(field).child(button("save", None, "Save token", Look::Primary).h(px(40.)).on_click(cx.listener(|this, _, _, cx| {
                this.save_token();
                cx.notify();
            }))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child("No token yet? Run this in a terminal:")
                    .child(
                        div()
                            .h(px(28.))
                            .pl_2p5()
                            .pr_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(6.))
                            .bg(t.surface)
                            .border_1()
                            .border_color(t.seam)
                            .child(div().font_family(CODE_FONT).text_color(t.fg).child("claude setup-token"))
                            .child(button("copy-cmd", Some(if self.copied { "check" } else { "copy" }), if self.copied { "Copied" } else { "Copy" }, Look::Quiet).h(px(22.)).px_1p5().text_size(px(12.5)).on_click(cx.listener(|this, _, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string("claude setup-token".into()));
                                this.copied = true;
                                window.focus(&this.token_focus);
                                cx.notify();
                            }))),
                    ),
            )
            .when_some(self.notice.clone(), |d, (msg, ok)| d.child(div().text_size(px(13.)).text_color(if ok { t.ok } else { t.bad }).child(msg)))
            .when(skip, |d| {
                d.child(div().pt_2().flex().child(button("skip-claude", None, "Skip for now", Look::Quiet).text_color(t.muted).on_click(cx.listener(|this, _, window, cx| {
                    this.claude_skipped = true;
                    if let Some(p) = std::env::home_dir().map(|h| h.join(".config/popcorn/claude-skipped")) {
                        std::fs::create_dir_all(p.parent().unwrap()).ok();
                        std::fs::write(p, "").ok();
                    }
                    this.notice = None;
                    window.focus(&this.focus);
                    cx.notify();
                }))))
            })
    }

    /// Add a Claude token after setup was skipped.
    fn connect_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.connecting {
            return None;
        }
        let t = theme();
        Some(
            div()
                .id("connect-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.32))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                    this.connecting = false;
                    window.focus(&this.focus);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(560.))
                        .p_6()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .bg(t.bg)
                        .rounded(px(10.))
                        .border_1()
                        .border_color(t.seam)
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).child("Connect Claude"))
                        .child(div().text_color(t.muted).line_height(px(21.)).child("Paste a long-lived token. It stays on this computer. Popcorn adds it to requests when they leave a corn."))
                        .child(self.token_form(false, window, cx)),
                )
                .into_any_element(),
        )
    }

    fn on_token_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        cx.stop_propagation();
        if k.modifiers.secondary() && k.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                self.token.push_str(text.trim());
            }
        } else if k.key == "backspace" {
            if k.modifiers.secondary() {
                self.token.clear();
            } else {
                self.token.pop();
            }
        } else if k.key == "escape" && self.connecting {
            self.connecting = false;
        } else if k.key == "enter" {
            self.save_token();
        } else if let Some(c) = k.key_char.as_deref().filter(|c| !k.modifiers.secondary() && !c.chars().any(char::is_control)) {
            self.token.push_str(c.trim());
        }
        self.notice = None;
        cx.notify();
    }

    fn save_token(&mut self) {
        match gate::save_credential(&self.token) {
            Ok(()) => {
                self.has_key = true;
                self.token.clear();
                self.notice = None;
                self.connecting = false;
            }
            Err(_) if self.token.is_empty() => self.notice = Some(("Paste the token first. It starts with sk-ant-.".into(), false)),
            Err(_) => self.notice = Some((format!("Tokens start with sk-ant-. This one starts with \"{}\".", self.token.chars().take(6).collect::<String>()), false)),
        }
    }

    fn share_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Share".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| this.send_folder(path, cx)).ok();
        })
        .detach();
    }

    /// Pick files and folders and copy them into the corn's ~/inbox.
    fn copy_in(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: true, multiple: true, prompt: Some("Copy in".into()) });
        let Some(c) = self.case() else { return };
        let (dir, log) = (c.dir.clone(), c.log.clone());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let r = cx
                .background_executor()
                .spawn(async move { paths.iter().try_for_each(|p| sandbox::send(&dir, &log, p)).map(|_| paths.len()) })
                .await;
            this.update(cx, |this, cx| {
                this.notice = Some(match r {
                    Ok(n) => (format!("Copied {n} item{} into ~/inbox.", if n == 1 { "" } else { "s" }), true),
                    Err(e) => (e, false),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(c) = self.case() else { return };
        let (dir, log) = (c.dir.clone(), c.log.clone());
        self.notice = Some(("Copying the folder into the corn…".into(), true));
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { share::send(&dir, &log, &path) }).await;
            this.update(cx, |this, cx| {
                this.notice = Some(match r {
                    Ok(n) => (format!("Copied {n} files. .git and ignored files stayed on your computer."), true),
                    Err(e) => (e, false),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn review_changes(&mut self, cx: &mut Context<Self>) {
        let corn = self.selected;
        let Some(c) = self.case() else { return };
        let dir = c.dir.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { share::pull(&dir) }).await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(changes) if changes.is_empty() => this.notice = Some(("No changes yet.".into(), true)),
                    Ok(changes) => {
                        this.notice = None;
                        this.review = Some(Review { corn, changes, open: None });
                    }
                    Err(e) => this.notice = Some((e, false)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Approve (write to your folder) or deny (leave yours as it is) one change.
    fn decide_change(&mut self, i: usize, approve: bool) {
        let Some(r) = self.review.as_mut() else { return };
        if i >= r.changes.len() {
            return;
        }
        let change = r.changes.remove(i);
        r.open = None;
        let Some(c) = self.cases.get(r.corn) else { return };
        if approve {
            if let Err(e) = share::apply(&c.dir, &c.log, &change) {
                self.notice = Some((e, false));
            }
        } else {
            gate::record(&c.log, &c.dir, Event::note(&format!("Kept your version of {}", change.rel)));
        }
        if self.review.as_ref().is_some_and(|r| r.changes.is_empty()) {
            self.review = None;
        }
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.case() else { return };
        let (dir, log) = (c.dir.clone(), c.log.clone());
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { sandbox::export(&dir, &log) }).await;
            this.update(cx, |this, cx| {
                this.notice = Some(match r {
                    Ok((_, 0)) => ("The outbox is empty. Ask Claude to save results in ~/outbox, then export again.".into(), false),
                    Ok((path, n)) => {
                        std::process::Command::new("xdg-open").arg(&path).spawn().ok();
                        (format!("Exported {n} file{} to {}", if n == 1 { "" } else { "s" }, path.display()), true)
                    }
                    Err(e) => (e, false),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn build_image(&mut self, cx: &mut Context<Self>) {
        self.build = Build::Running("Starting…".into());
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/build-image.sh");
        let log = sandbox::home().join("build.log");
        cx.spawn(async move |this, cx| {
            let l = log.clone();
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = done.clone();
            let task = cx.background_executor().spawn(async move {
                let _set = Finish(flag);
                std::fs::create_dir_all(sandbox::home()).ok();
                let out = std::fs::File::create(&l).map_err(|e| e.to_string())?;
                let status = std::process::Command::new(script).stdout(out.try_clone().unwrap()).stderr(out).status().map_err(|e| e.to_string())?;
                if status.success() { Ok(()) } else { Err("The image build stopped. Its log is in build.log.".to_string()) }
            });
            // Show the last line of the build log while it runs.
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let last = std::fs::read_to_string(&log).unwrap_or_default().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(120).collect::<String>();
                this.update(cx, |this, cx| {
                    this.build = Build::Running(last);
                    cx.notify();
                })
                .ok();
            }
            let r = task.await;
            this.update(cx, |this, cx| {
                this.build = match r {
                    Ok(()) => Build::Idle,
                    Err(e) => Build::Failed(e),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---------- title bar: brand, case tabs, window controls ----------

    fn titlebar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        let mac = cfg!(target_os = "macos");
        let controls = window.window_controls();
        let maximized = window.is_maximized();
        let dark = self.appearance.is_dark(window.appearance());
        let ready = sandbox::golden().exists() && (self.has_key || self.claude_skipped) && self.has_name;
        let selected = self.selected;
        let tabs: Vec<AnyElement> = if !ready { Vec::new() } else { self
            .cases
            .iter_mut()
            .enumerate()
            .map(|(i, c)| {
                let on = i == selected;
                let running = c.running();
                let waiting = !c.policy.asks.lock().unwrap().is_empty();
                // Status only when something is happening: a heartbeat while it runs, a pulsing
                // hand while it waits for your answer. A stopped case is just its name.
                let status = if waiting {
                    Some(ui_icon("hand").size(px(14.)).text_color(t.signal).with_animation(("wait", i), Animation::new(Duration::from_millis(1100)).repeat().with_easing(pulsating_between(0.35, 1.)), |el, k| el.opacity(k)).into_any_element())
                } else if running {
                    Some(ui_icon("activity").size(px(14.)).text_color(t.ok).into_any_element())
                } else {
                    None
                };
                let label = c.name.clone();
                let group: SharedString = format!("tab-{i}").into();
                div()
                    .id(("tab", i))
                    .group(group.clone())
                    .h(px(30.))
                    .px_3()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .when(on, |d| d.bg(t.bg).font_weight(FontWeight::MEDIUM).border_1().border_color(t.seam))
                    .when(!on, |d| d.text_color(t.muted).hover(|s| s.bg(t.hover).text_color(t.fg)))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if e.click_count == 2 {
                            let name = this.cases.get(i).map(|c| c.name.clone()).unwrap_or_default();
                            this.renaming = Some((i, name));
                            window.focus(&this.rename_focus);
                        }
                        this.selected = i;
                        this.notice = None;
                        cx.notify();
                    }))
                    .children(status)
                    // Long names are cut with an ellipsis so tabs stay compact; the full name is in the header.
                    .child(div().when(label.chars().count() > 22, |d| d.w(px(160.))).truncate().child(label))
                    .child(
                        div()
                            .id(("close-tab", i))
                            .size(px(18.))
                            .ml_0p5()
                            .mr(px(-6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .when(!on, |d| d.invisible().group_hover(group.clone(), |s| s.visible()))
                            .hover(|s| s.bg(t.selected))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.deleting = Some(i);
                                window.focus(&this.focus);
                                cx.notify();
                            }))
                            .child(ui_icon("x").size(px(12.)).text_color(t.muted)),
                    )
                    .into_any_element()
            })
            .collect() };
        div()
            .id("titlebar")
            .h(px(46.))
            .flex_shrink_0()
            .flex()
            .items_center()
            // macOS draws its own traffic lights in this gap.
            .pl(if mac { px(84.) } else { px(14.) })
            .pr(px(7.))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.should_move = true))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.should_move = false))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|e, window, _| {
                if e.click_count() == 2 {
                    window.zoom_window();
                }
            })
            .on_mouse_down(MouseButton::Right, |e, window, _| window.show_window_menu(e.position))
            .when(!mac, |d| {
                d.child(div().pr_5().flex().items_center().gap_2().child(div().text_size(px(19.)).line_height(px(22.)).child("🍿")).child(div().text_size(px(15.5)).font_weight(FontWeight::SEMIBOLD).child("Popcorn")))
            })
            .child(div().id("tabs").flex_1().min_w_0().flex().items_center().gap_1().overflow_x_scroll().children(tabs).when(ready, |d| {
                d.child(div().flex().items_center().child(control_like("new-case", "plus", cx.listener(|this, _, _, cx| {
                    this.new_case();
                    cx.notify();
                }))))
            }))
            .child(div().flex().items_center().gap_0p5().child(control_like("theme", if dark { "sun" } else { "moon" }, cx.listener(|this, _, window, cx| {
                this.appearance = if this.appearance.is_dark(window.appearance()) { Appearance::Light } else { Appearance::Dark };
                this.appearance.save();
                this.apply_appearance(window);
                cx.notify();
            }))).when(!mac, |d| {
                d.when(controls.minimize, |d| d.child(control("min", "minus", WindowControlArea::Min, false, |w| w.minimize_window())))
                    .when(controls.maximize, |d| d.child(control("max", if maximized { "copy" } else { "square" }, WindowControlArea::Max, false, |w| w.zoom_window())))
                    .child(control("close", "x", WindowControlArea::Close, true, |w| w.remove_window()))
            }))
    }

    // ---------- first run ----------

    fn setup(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme();
        let image = sandbox::golden().exists();
        if image && (self.has_key || self.claude_skipped) && self.has_name {
            return None;
        }
        if !self.intro_seen {
            return Some(self.intro(cx));
        }
        let step = |n: usize, done: bool, title: &str, body: &str| {
            div()
                .flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .mt(px(1.))
                        .size(px(26.))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(done, |d| d.bg(t.ok).child(ui_icon("check").size(px(15.)).text_color(t.bg)))
                        .when(!done, |d| d.border_1().border_color(t.fg).text_color(t.fg).child(n.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).when(done, |d| d.text_color(t.muted)).child(title.to_string()))
                        .when(!done && !body.is_empty(), |d| d.child(div().text_color(t.muted).line_height(px(21.)).child(body.to_string()))),
                )
        };
        let build = match &self.build {
            Build::Running(line) => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(t.muted)
                .child(ui_icon("hammer").size(px(15.)).text_color(t.muted))
                .child(div().font_family(CODE_FONT).truncate().child(if line.is_empty() { "Building…".to_string() } else { line.clone() }))
                .into_any_element(),
            Build::Failed(e) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_color(t.bad).child(e.clone()))
                .child(button("build", Some("hammer"), "Build again", Look::Primary).on_click(cx.listener(|this, _, _, cx| this.build_image(cx))))
                .into_any_element(),
            Build::Idle => button("build", Some("hammer"), "Build the image", Look::Primary).on_click(cx.listener(|this, _, _, cx| this.build_image(cx))).into_any_element(),
        };

        // The token field: a real input that takes typing and paste, and never shows the secret.
        // Your name, shown as the account inside every case.
        let name_focused = self.name_focus.is_focused(window);
        let name_field = div()
            .id("name")
            .track_focus(&self.name_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_name_key(e, window, cx)))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.name_focus);
                cx.notify();
            }))
            .flex_1()
            .min_w_0()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(t.bg)
            .border_1()
            .border_color(if name_focused { t.fg } else { t.seam })
            .cursor(CursorStyle::IBeam)
            .text_size(px(14.5))
            .child(if self.name.is_empty() && !name_focused { div().text_color(t.faint).child("Your first name").into_any_element() } else { div().truncate().child(self.name.clone()).into_any_element() })
            .when(name_focused, |d| d.child(div().w(px(1.5)).h(px(18.)).bg(t.fg)));

        Some(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(560.))
                        .flex()
                        .flex_col()
                        .gap_8()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_size(px(48.)).line_height(px(56.)).mb_2().child("🍿"))
                                .child(div().text_size(px(30.)).font_weight(FontWeight::SEMIBOLD).line_height(px(36.)).child("Set up your workbench"))
                                .child(div().text_size(px(15.)).text_color(t.muted).line_height(px(23.)).child("A place for risky work. Each corn runs in its own microVM with its own kernel, and cannot see your files, your SSH keys or your network. Three steps, once.")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(step(1, self.has_name, "What should corns call you?", ""))
                                .when(!self.has_name, |d| {
                                    d.child(
                                        div()
                                            .pl(px(42.))
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(div().flex().gap_2().child(name_field).child(button("save-name", None, "Continue", Look::Primary).h(px(40.)).on_click(cx.listener(|this, _, window, cx| {
                                                this.save_name(window);
                                                cx.notify();
                                            }))))
                                            .when_some(self.notice.clone().filter(|(_, ok)| !ok), |d, (msg, _)| d.child(div().text_size(px(13.)).text_color(t.bad).child(msg))),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(step(2, image, "Build the workbench image", "Debian with Node, Python, git, curl, strace and Claude Code, ready in every corn. Built with Docker on this computer in a few minutes."))
                                .when(!image, |d| d.child(div().pl(px(42.)).flex().child(build))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(step(3, self.has_key, "Connect Claude", "Get a long-lived token and paste it below. It stays on this computer; Popcorn adds it to requests as they leave a corn, so code inside can never read it."))
                                .when(self.has_name && !self.has_key, |d| {
                                    d.child(div().pl(px(42.)).child(self.token_form(true, window, cx)))
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---------- a case ----------

    fn empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(480.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(img("icons/popcorn.png").size(px(56.)).mb_3())
                    .child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).child("Pop your first corn"))
                    .child(div().text_size(px(15.)).text_color(t.muted).line_height(px(23.)).child("A corn is a disposable machine for one risky job: untrusted code, a suspicious download, a binary you don't trust. Whatever runs in it stays in it."))
                    .child(div().mt_4().flex().child(button("first", Some("plus"), "New corn", Look::Primary).on_click(cx.listener(|this, _, _, cx| {
                        this.new_case();
                        cx.notify();
                    })))),
            )
            .into_any_element()
    }

    /// The hazard band: the case is holding a connection open until you decide.
    fn hazard(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme();
        let ink: Hsla = rgb(0x1c2124).into();
        let asks: Vec<(String, u64)> = self.cases.get(self.selected)?.policy.asks.lock().unwrap().iter().map(|a| (a.target.clone(), a.at)).collect();
        let (target, at) = asks.first()?.clone();
        let more = asks.len() - 1;
        let left = gate::ASK_TIMEOUT.as_secs().saturating_sub(now().saturating_sub(at));
        let (host, port) = target.rsplit_once(':').map(|(h, p)| (h.to_string(), p.to_string())).unwrap_or((target.clone(), String::new()));
        let decide = |id: &'static str, label: &'static str, k: &'static str, a: Answer| {
            div()
                .id(id)
                .h(px(34.))
                .pl_3()
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ink)
                .when(a == Answer::Deny, |d| d.bg(ink).text_color(t.signal))
                .when(a != Answer::Deny, |d| d.border_1().border_color(ink.opacity(0.4)).hover(|s| s.bg(ink.opacity(0.08))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_first(a);
                    cx.notify();
                }))
                .child(label)
                .child(if a == Answer::Deny { key(k, ink, t.signal) } else { key(k, gpui::transparent_black(), ink) })
        };
        Some(
            div()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .bg(t.signal)
                .text_color(ink)
                .child(stripes(ink).h(px(6.)).w_full())
                .child(
                    div()
                        .px_8()
                        .py_4()
                        .flex()
                        .items_center()
                        .gap_6()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(div().text_size(px(13.)).child("This corn is trying to connect to"))
                                .child(
                                    div()
                                        .flex()
                                        .items_baseline()
                                        .gap_1()
                                        .child(div().font_family(CODE_FONT).text_size(px(24.)).font_weight(FontWeight::MEDIUM).truncate().child(host))
                                        .when(port != "443", |d| d.child(div().font_family(CODE_FONT).text_size(px(16.)).opacity(0.7).child(format!(":{port}")))),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .opacity(0.75)
                                        .child(format!("Held open. Refused in {left}s if you don't answer{}.", if more > 0 { format!(" · {more} more waiting") } else { String::new() })),
                                ),
                        )
                        .child(decide("deny", "Deny", "D", Answer::Deny))
                        .child(decide("once", "Allow once", "O", Answer::Once))
                        .child(decide("always", "Always allow", "A", Answer::Always)),
                )
                .into_any_element(),
        )
    }

    fn changes_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme();
        let r = self.review.as_ref().filter(|r| r.corn == self.selected)?;
        let folder = self.cases.get(r.corn).and_then(|c| share::get(&c.dir)).map(|s| s.name).unwrap_or_default();
        let rows = r.changes.iter().enumerate().map(|(i, c)| {
            let (label, color) = match c.kind {
                share::Kind::Added => ("Added", t.ok),
                share::Kind::Changed => ("Changed", t.fg),
                share::Kind::Deleted => ("Deleted", t.bad),
            };
            let open = r.open.as_ref().filter(|(o, _)| *o == i).map(|(_, d)| d.clone());
            let rel = c.rel.clone();
            div()
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(t.seam.opacity(0.6))
                .child(
                    div()
                        .id(("change", i))
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_4()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(r) = this.review.as_mut() else { return };
                            r.open = match r.open.take() {
                                Some((o, _)) if o == i => None,
                                _ => {
                                    let d = this.cases.get(r.corn).map(|c| share::diff(&c.dir, &r.changes[i])).unwrap_or_default();
                                    Some((i, d))
                                }
                            };
                            cx.notify();
                        }))
                        .child(ui_icon(if open.is_some() { "chevron-down" } else { "chevron-right" }).size(px(14.)).text_color(t.faint))
                        .child(div().w(px(64.)).flex_shrink_0().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(color).child(label))
                        .child(div().flex_1().min_w_0().truncate().font_family(CODE_FONT).text_size(px(13.)).child(rel))
                        .when(c.host_moved, |d| d.child(div().flex_shrink_0().text_size(px(12.)).text_color(t.signal).child("Also changed on your side")))
                        .child(button(("deny-change", i), None, "Deny", Look::Quiet).h(px(28.)).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.decide_change(i, false);
                            cx.notify();
                        })))
                        .child(button(("approve-change", i), Some("check"), "Approve", Look::Plain).h(px(28.)).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.decide_change(i, true);
                            cx.notify();
                        }))),
                )
                .when_some(open, |d, diff| {
                    d.child(
                        div()
                            .id(("diff", i))
                            .mb_3()
                            .max_h(px(380.))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded(px(6.))
                            .bg(t.surface)
                            .font_family(CODE_FONT)
                            .text_size(px(12.5))
                            .children(diff.lines().map(|l| {
                                let color = if l.starts_with('+') { t.ok } else if l.starts_with('-') { t.bad } else if l.starts_with("@@") { t.faint } else { t.muted };
                                div().whitespace_nowrap().text_color(color).child(if l.is_empty() { " ".to_string() } else { l.to_string() })
                            }))
                            .when(diff.is_empty(), |d| d.child(div().text_color(t.muted).child("Binary or empty file."))),
                    )
                })
        });
        Some(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .pb_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(format!("Changes to {folder}")))
                        .child(button("done-review", None, "Done", Look::Quiet).on_click(cx.listener(|this, _, _, cx| {
                            this.review = None;
                            cx.notify();
                        })))
                        .child(button("approve-all", Some("check"), "Approve all", Look::Primary).on_click(cx.listener(|this, _, _, cx| {
                            while this.review.as_ref().is_some_and(|r| !r.changes.is_empty()) {
                                this.decide_change(0, true);
                            }
                            cx.notify();
                        }))),
                )
                .child(div().pb_3().text_size(px(13.)).text_color(t.muted).child("Only what you approve is written to your folder."))
                .children(rows)
                .into_any_element(),
        )
    }

    fn timeline(&self, events: &[Event], running: bool) -> AnyElement {
        let t = theme();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().pb_3().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("What happened"))
            .when(events.is_empty(), |d| {
                d.child(
                    div()
                        .py_8()
                        .text_color(t.muted)
                        .line_height(px(22.))
                        .child(if running { "Nothing has crossed the airlock yet. Open a terminal, drop files anywhere on this window, and get to work." } else { "Start the corn. Everything it sends or receives will show up here." }),
                )
            })
            .children(events.iter().rev().take(300).map(|e| {
                let (verdict, color): (&str, Hsla) = match e.kind {
                    Kind::Blocked => ("Refused", t.bad),
                    Kind::Allowed => ("Allowed", t.ok),
                    Kind::Claude => ("Claude", t.fg),
                    Kind::Note => ("", t.muted),
                };
                div()
                    .py_2()
                    .flex()
                    .items_baseline()
                    .gap_4()
                    .border_b_1()
                    .border_color(t.seam.opacity(0.6))
                    .child(div().w(px(36.)).flex_shrink_0().text_size(px(12.)).font_family(CODE_FONT).text_color(t.faint).child(ago(e.at)))
                    .child(div().w(px(64.)).flex_shrink_0().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(color).child(verdict))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(e.kind == Kind::Note, |d| d.text_color(t.muted))
                            .when(e.kind != Kind::Note, |d| d.font_family(CODE_FONT).text_size(px(13.)))
                            .child(e.text.clone()),
                    )
            }))
            .into_any_element()
    }

    /// The boundary between this computer and the case, lane by lane, with what crossed it.
    fn airlock(&self, c: &Case, x: &Crossings, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let sealed = c.mode == Mode::Sealed;
        let mine: Vec<String> = c.policy.allow.lock().unwrap().iter().cloned().collect();
        let lane = |icon: &'static str, name: &str, rule: &str, count: usize, color: Hsla| {
            div()
                .py_3()
                .flex()
                .items_start()
                .gap_3()
                .child(div().mt(px(2.)).child(ui_icon(icon).size(px(16.)).text_color(t.muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::MEDIUM).child(name.to_string()))
                        .child(div().text_size(px(12.5)).text_color(t.muted).line_height(px(18.)).child(rule.to_string())),
                )
                .child(div().font_family(CODE_FONT).text_size(px(15.)).font_weight(FontWeight::MEDIUM).text_color(if count > 0 { color } else { t.faint }).child(count.to_string()))
        };
        div()
            .w(px(330.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(div().pb_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("The airlock"))
            .child(div().pb_3().text_size(px(13.)).text_color(t.muted).child("Everything between this computer and the corn passes here."))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(if self.has_key {
                        lane("sparkles", "Claude", "Always on, through its own channel. Your token is added out here, never inside.", x.claude, t.fg)
                    } else {
                        lane("sparkles", "Claude", "Not connected. Corns work without it.", x.claude, t.faint).child(button("connect", None, "Connect", Look::Plain).h(px(28.)).on_click(cx.listener(|this, _, window, cx| {
                            this.connecting = true;
                            this.notice = None;
                            window.focus(&this.token_focus);
                            cx.notify();
                        })))
                    })
                    .child(lane("network", "Network", if sealed { "Every connection is refused and logged." } else { "Every new host waits for your answer." }, x.allowed + x.refused, if x.refused > 0 { t.bad } else { t.ok }))
                    .child(lane("upload", "Files in", "Dropped here, they land in ~/inbox.", x.files_in, t.fg))
                    .child(lane("folder-lock", "Files out", "Only what you export from ~/outbox, as inert copies.", x.files_out, t.fg)),
            )
            .child(div().pt_6().pb_2().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Shared folder"))
            .child(match share::get(&c.dir) {
                None => div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(px(13.)).text_color(t.faint).child("The corn gets a copy. Changes come back only when you approve them."))
                    .child(div().flex().child(button("share", Some("folder-lock"), "Share a folder", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                        if this.case().is_some_and(|c| c.running()) {
                            this.share_folder(cx);
                        } else {
                            this.notice = Some(("Start the corn, then share a folder.".into(), false));
                        }
                        cx.notify();
                    }))))
                    .into_any_element(),
                Some(sh) => {
                    let host = sh.host.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().font_family(CODE_FONT).text_size(px(13.)).child(format!("~/work/{}", sh.name)))
                        .child(div().text_size(px(12.)).text_color(t.faint).truncate().child(format!("From {}", sh.host.display())))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(button("review", Some("check"), "Review changes", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.review_changes(cx);
                                    cx.notify();
                                })))
                                .child(button("resend", None, "Send again", Look::Quiet).on_click(cx.listener(move |this, _, _, cx| {
                                    this.send_folder(host.clone(), cx);
                                    cx.notify();
                                }))),
                        )
                        .into_any_element()
                }
            })
            .child(div().pt_6().pb_2().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Always allowed"))
            .when(mine.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(t.faint).child("Hosts you always allow show up here.")))
            .children(mine.into_iter().enumerate().map(|(i, h)| {
                let host = h.clone();
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().font_family(CODE_FONT).text_size(px(13.)).child(h))
                    .child(button(("forget", i), None, "Remove", Look::Quiet).h(px(24.)).text_color(t.muted).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(c) = this.cases.get(this.selected) {
                            c.policy.forget(&host);
                        }
                        cx.notify();
                    })))
            }))
            .into_any_element()
    }

    fn case_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let hazard = self.hazard(cx);
        let notice = self.notice.clone();
        let Some(case) = self.cases.get_mut(self.selected) else { return self.empty(cx) };
        let running = case.running();
        let started = case.started;
        let mode = case.mode;
        let name = case.name.clone();
        let events: Vec<Event> = case.log.lock().unwrap().clone();
        let x = crossings(&events);
        let airlock = self.airlock(&self.cases[self.selected], &x, cx);
        let timeline = match self.changes_view(cx) {
            Some(changes) => changes,
            None => self.timeline(&events, running),
        };

        let network = |m: Mode, label: &'static str| {
            let on = mode == m;
            div()
                .id(label)
                .h(px(28.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(5.))
                .cursor_pointer()
                .text_size(px(13.))
                .when(on, |d| d.bg(t.bg).font_weight(FontWeight::MEDIUM).border_1().border_color(t.seam))
                .when(!on, |d| d.text_color(t.muted).hover(|s| s.text_color(t.fg)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(c) = this.case() {
                        c.set_mode(m);
                    }
                    cx.notify();
                }))
                .child(label)
        };

        let header = div()
            .px_8()
            .pt_7()
            .pb_5()
            .flex()
            .items_end()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(match self.renaming.clone() {
                        Some((r, text)) if r == self.selected => div()
                            .id("rename")
                            .track_focus(&self.rename_focus)
                            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_rename_key(e, window, cx)))
                            .h(px(40.))
                            .w(px(560.))
                            .px_2()
                            .ml(px(-9.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(t.fg)
                            .bg(t.surface)
                            .text_size(px(28.))
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            // Left-aligned while it fits; once it is longer than the box, keep the end
                            // (where you are typing) in view.
                            // ponytail: "fits" is a character count for this font size, not a measurement.
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .flex()
                                    .items_center()
                                    .when(text.chars().count() > 20, |d| d.justify_end())
                                    .child(div().flex_none().whitespace_nowrap().child(text))
                                    .child(div().flex_none().ml(px(1.)).w(px(2.)).h(px(24.)).bg(t.fg)),
                            )
                            .child(div().flex_shrink_0().pl_3().text_size(px(12.)).font_weight(FontWeight::NORMAL).font_family(CODE_FONT).text_color(t.faint).child("Enter / Esc"))
                            .into_any_element(),
                        _ => div()
                            .id("title")
                            .group("title")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.cases.get(this.selected).map(|c| c.name.clone()).unwrap_or_default();
                                this.renaming = Some((this.selected, name));
                                window.focus(&this.rename_focus);
                                cx.notify();
                            }))
                            .child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).line_height(px(32.)).child(name))
                            .child(ui_icon("pencil").size(px(16.)).text_color(t.faint).invisible().group_hover("title", |s| s.visible()))
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(13.))
                                    .text_color(t.muted)
                                    .when(running, |d| d.child(ui_icon("activity").size(px(14.)).text_color(t.ok)))
                                    .child(match (running, started) {
                                        (true, Some(s)) => running_for(s.elapsed()),
                                        (true, None) => "Running".into(),
                                        _ => "Stopped. The disk is kept until you delete the corn.".into(),
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(13.))
                                    .text_color(t.muted)
                                    .child("Network")
                                    .child(div().p(px(2.)).flex().gap_0p5().rounded(px(7.)).bg(t.surface).child(network(Mode::Ask, "Ask before connecting")).child(network(Mode::Sealed, "Sealed"))),
                            ),
                    ),
            )
            .when(running, |d| {
                d.child(button("stop", Some("square-stop"), "Stop", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(c) = this.case() {
                        c.stop();
                    }
                    cx.notify();
                })))
                .child(button("copy-in", Some("upload"), "Copy in", Look::Plain).on_click(cx.listener(|this, _, _, cx| this.copy_in(cx))))
                .child(button("export", Some("package"), "Export outbox", Look::Plain).on_click(cx.listener(|this, _, _, cx| this.export(cx))))
                .child(button("term", Some("terminal"), "Open terminal", Look::Primary).on_click(cx.listener(|this, _, _, cx| {
                    let r = this.case().map(|c| c.open_terminal()).unwrap_or(Ok(()));
                    this.report(r);
                    cx.notify();
                })))
            })
            .when(!running, |d| {
                d.child(button("delete", Some("trash-2"), "Delete", Look::Plain).on_click(cx.listener(|this, _, window, cx| {
                    this.deleting = Some(this.selected);
                    window.focus(&this.focus);
                    cx.notify();
                })))
                .child(button("start", Some("play"), "Start", Look::Primary).on_click(cx.listener(|this, _, _, cx| {
                    let r = this.case().map(|c| c.start()).unwrap_or(Ok(()));
                    this.report(r);
                    cx.notify();
                })))
            });

        div()
            .id("case")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(theme().ok.opacity(0.06)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let Some(c) = this.case() else { return };
                if !c.running() {
                    this.notice = Some(("Start the corn, then drop the files again.".into(), false));
                    cx.notify();
                    return;
                }
                if !c.has_title() {
                    if let Some(first) = paths.paths().first().and_then(|p| p.file_name()) {
                        let n = first.to_string_lossy();
                        // left-pad-1.3.0.tgz -> left-pad-1.3.0
                        let stem = n.trim_end_matches(".tgz").trim_end_matches(".tar.gz").trim_end_matches(".whl").trim_end_matches(".zip").to_string();
                        c.rename(&stem);
                    }
                }
                let (dir, log) = (c.dir.clone(), c.log.clone());
                for p in paths.paths().to_vec() {
                    let (dir, log) = (dir.clone(), log.clone());
                    std::thread::spawn(move || {
                        if let Err(e) = sandbox::send(&dir, &log, &p) {
                            gate::record(&log, &dir, Event::note(&format!("Could not copy {}: {e}", p.display())));
                        }
                    });
                }
            }))
            .child(header)
            .when_some(notice, |d, (msg, ok)| d.child(div().px_8().pb_4().text_size(px(13.)).text_color(if ok { t.ok } else { t.bad }).child(msg)))
            .children(hazard)
            .child(div().h(px(1.)).flex_shrink_0().bg(t.seam))
            .child(div().id("case-body").flex_1().min_h_0().overflow_y_scroll().px_8().py_6().flex().gap_12().child(timeline).child(airlock))
            .into_any_element()
    }
}

/// Sets the flag when dropped, so the build loop sees the end however the task exits.
struct Finish(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for Finish {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Render for Popcorn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        // While a name is being edited, the box owns the keyboard, so typing, Enter and Escape reach it.
        if self.renaming.is_some() && !self.rename_focus.is_focused(window) {
            window.focus(&self.rename_focus);
        }
        let body = match self.setup(window, cx) {
            Some(setup) => setup,
            None => self.case_view(cx),
        };
        let panel = div()
            .flex_1()
            .min_h_0()
            .mx(px(8.))
            .mb(px(8.))
            .flex()
            .flex_col()
            .bg(t.bg)
            .rounded(PANEL_RADIUS)
            .overflow_hidden()
            .child(body);
        let app = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.on_key(e, cx)))
            .flex()
            .flex_col()
            .bg(t.chrome)
            .font_family(UI_FONT)
            .text_size(px(14.))
            .text_color(t.fg)
            .child(self.titlebar(window, cx))
            .child(panel)
            .children(self.delete_dialog(cx))
            .children(self.connect_dialog(window, cx));

        // Server decorations (macOS, Windows, KDE): the platform owns the frame.
        let Decorations::Client { tiling } = window.window_decorations() else {
            window.set_client_inset(px(0.));
            return app.into_any_element();
        };
        // Client decorations (GNOME and other Wayland/X11): draw the rounded frame, shadow and resize band ourselves.
        window.set_client_inset(INSET);
        let round = |d: Div| {
            d.when(!(tiling.top || tiling.left), |d| d.rounded_tl(WINDOW_RADIUS))
                .when(!(tiling.top || tiling.right), |d| d.rounded_tr(WINDOW_RADIUS))
                .when(!(tiling.bottom || tiling.left), |d| d.rounded_bl(WINDOW_RADIUS))
                .when(!(tiling.bottom || tiling.right), |d| d.rounded_br(WINDOW_RADIUS))
        };
        div()
            .id("frame")
            .size_full()
            .when(!tiling.top, |d| d.pt(INSET))
            .when(!tiling.bottom, |d| d.pb(INSET))
            .when(!tiling.left, |d| d.pl(INSET))
            .when(!tiling.right, |d| d.pr(INSET))
            .child(
                canvas(
                    |_, window, _| window.insert_hitbox(Bounds::new(point(px(0.), px(0.)), window.window_bounds().get_bounds().size), HitboxBehavior::Normal),
                    |_, hitbox, window, _| {
                        let size = window.window_bounds().get_bounds().size;
                        let Some(edge) = resize_edge(window.mouse_position(), size) else { return };
                        let cursor = match edge {
                            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
                            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
                        };
                        window.set_cursor_style(cursor, &hitbox);
                    },
                )
                .size_full()
                .absolute(),
            )
            .on_mouse_move(|_, window, _| window.refresh())
            .on_mouse_down(MouseButton::Left, |e, window, _| {
                if let Some(edge) = resize_edge(e.position, window.window_bounds().get_bounds().size) {
                    window.start_window_resize(edge);
                }
            })
            .child(
                round(app)
                    .cursor(CursorStyle::Arrow)
                    .border_1()
                    .border_color(t.frame)
                    .when(!tiling.is_tiled(), |d| {
                        d.shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.22), offset: point(px(0.), px(2.)), blur_radius: INSET / 1.5, spread_radius: px(0.) }])
                    })
                    .on_mouse_move(|_, _, cx| cx.stop_propagation()),
            )
            .into_any_element()
    }
}

impl Drop for Popcorn {
    /// Closing Popcorn stops every case; nothing keeps running unseen.
    fn drop(&mut self) {
        self.cases.iter_mut().for_each(Case::stop);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("attach") {
        sandbox::attach(std::path::Path::new(args.get(2).map(String::as_str).unwrap_or(".")));
    }
    Application::new().with_assets(icons::Assets).run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf").as_slice()),
            ])
            .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1300.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Popcorn".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(16.))),
                }),
                window_decorations: Some(WindowDecorations::Client),
                // Linux needs real transparency so the rounded corners and shadow band show the desktop.
                window_background: if cfg!(target_os = "linux") { WindowBackgroundAppearance::Transparent } else { WindowBackgroundAppearance::Opaque },
                window_min_size: Some(size(px(1000.), px(620.))),
                app_id: Some("popcorn".into()),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Popcorn::new(window, cx));
                // Focus the token field on first run, so typing or pasting works right away.
                // Focus the first field still to fill, so typing works right away.
                let (n, k, name, token, focus) = view.read_with(cx, |v, _| (v.has_name, v.has_key, v.name_focus.clone(), v.token_focus.clone(), v.focus.clone()));
                window.focus(if !n { &name } else if !k { &token } else { &focus });
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
