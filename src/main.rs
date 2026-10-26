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
    settings: bool,
    /// The Erase button was pressed once; the next press erases.
    confirm_erase: bool,
    /// The setup command was just copied; the button says so.
    copied: bool,
    focus: FocusHandle,
    /// Last error or confirmation, shown in context.
    notice: Option<(String, bool)>,
    /// The case the delete dialog is asking about.
    deleting: Option<usize>,
    /// Close was pressed while a terminal is open; the close dialog asks first.
    closing: bool,
    /// The shared folder waiting for a confirm before it replaces the corn's copy.
    refreshing: Option<PathBuf>,
    /// The intro page was dismissed this session.
    intro_seen: bool,
    /// Changes pulled back from a corn's shared folder, waiting for your decision.
    review: Option<Review>,
    /// The tab being renamed, with the text typed so far.
    renaming: Option<(usize, String)>,
    rename_focus: FocusHandle,
    /// The two scroll areas of a corn page: the timeline (or review) and the airlock column.
    left_scroll: ScrollHandle,
    /// Timeline filter: text to match, and how far back to look (seconds; 0 is all).
    filter: String,
    filter_focus: FocusHandle,
    span: u64,
    right_scroll: ScrollHandle,
    should_move: bool,
    appearance: Appearance,
    _appearance_watch: Subscription,
    /// The newest question you were notified about. Question ids only grow.
    pinged: u64,
}

fn ui_icon(name: &str) -> Svg {
    svg().path(format!("icons/ui/{name}.svg")).flex_shrink_0()
}

/// Window button for Linux and Windows. Stops mouse-down so the title bar never starts a drag
/// from it: on Wayland a drag hands the pointer to the compositor and the click would be lost.
fn control(id: &'static str, icon: &'static str, area: WindowControlArea, danger: bool, action: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
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
            action(window, cx);
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
    button_tinted(id, icon, None, label, look)
}

/// A button whose icon has its own color, for example a muted red for Stop.
fn button_tinted(id: impl Into<ElementId>, icon: Option<&'static str>, tint: Option<Hsla>, label: &str, look: Look) -> Stateful<Div> {
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
        .when_some(icon, |d, i| d.child(ui_icon(i).size(px(15.)).text_color(tint.unwrap_or(fg))))
        .child(label.to_string())
}

/// A column that scrolls on its own, with a thin themed bar while you hover an overflowing column. Scroll
/// with the wheel or touchpad.
/// ponytail: the bar does not take drags; add a drag handler if people reach for it.
fn scroll_column(id: &'static str, handle: &ScrollHandle, content: AnyElement, cx: &mut Context<Popcorn>) -> Div {
    let t = theme();
    let view = handle.bounds().size.height;
    let max = handle.max_offset().height;
    let bar = (max > px(0.) && view > px(0.)).then(|| {
        let thumb = (view * (view / (view + max))).max(px(28.));
        let top = (view - thumb) * (-handle.offset().y / max).clamp(0., 1.);
        div().absolute().right(px(-10.)).top(top).w(px(4.)).h(thumb).rounded(px(2.)).bg(t.faint.opacity(0.4)).invisible().group_hover(id, |s| s.visible())
    });
    div()
        .group(id)
        .relative()
        .h_full()
        .min_h_0()
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .track_scroll(handle)
                // Redraw so the bar follows the content.
                .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                .child(content),
        )
        .children(bar)
}

/// A button with only an icon, no outline. The tooltip names it.
fn icon_button(id: &'static str, icon: &'static str, tint: Hsla, tip: &'static str) -> Stateful<Div> {
    let t = theme();
    div()
        .id(id)
        .size(px(34.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .tooltip(move |_, cx| cx.new(|_| Tip(tip)).into())
        .child(ui_icon(icon).size(px(15.)).text_color(tint))
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

/// Newest first, with runs of the same event folded into one entry and a count.
fn stacked_refs<'a>(events: &[&'a Event]) -> Vec<(&'a Event, usize)> {
    let mut out: Vec<(&Event, usize)> = Vec::new();
    for &e in events.iter().rev() {
        match out.last_mut() {
            Some((last, n)) if last.kind == e.kind && last.text == e.text => *n += 1,
            _ => out.push((e, 1)),
        }
    }
    out
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
        s if s < 60 => "Running. Started less than 1 min ago.".into(),
        s if s < 3600 => format!("Running. Started {} min ago.", s / 60),
        s => format!("Running. Started {} h {} min ago.", s / 3600, (s % 3600) / 60),
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
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let tick = this.update_in(cx, |this, window, cx| {
                    this.ping_new_asks(window.is_window_active());
                    cx.notify();
                });
                if tick.is_err() {
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
            settings: false,
            confirm_erase: false,
            copied: false,
            focus: cx.focus_handle(),
            notice: None,
            deleting: None,
            closing: false,
            refreshing: None,
            intro_seen: false,
            review: None,
            renaming: None,
            rename_focus: cx.focus_handle(),
            left_scroll: ScrollHandle::new(),
            filter: String::new(),
            filter_focus: cx.focus_handle(),
            span: 0,
            right_scroll: ScrollHandle::new(),
            should_move: false,
            appearance: Appearance::load(),
            _appearance_watch: cx.observe_window_appearance(window, |this, window, cx| {
                this.apply_appearance(window);
                cx.notify();
            }),
            pinged: 0,
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

    /// A desktop notification with a sound for each new question, while Popcorn is not in front.
    /// The page already shows the question when you look at it.
    fn ping_new_asks(&mut self, active: bool) {
        let (seen, mut new) = (self.pinged, Vec::new());
        for c in &self.cases {
            for a in c.policy.asks.lock().unwrap().iter().filter(|a| a.id > seen) {
                self.pinged = self.pinged.max(a.id);
                new.push((c.name.clone(), a.target.clone()));
            }
        }
        if active || new.is_empty() {
            return;
        }
        let (corn, target) = &new[0];
        let body = if new.len() == 1 { format!("{corn} wants to connect to {target}.") } else { format!("{} connections wait for your answer.", new.len()) };
        // ponytail: needs notify-send (libnotify) on the desktop; without it, no ping.
        std::thread::spawn(move || {
            std::process::Command::new("notify-send")
                .args(["--app-name=Popcorn", "--icon=popcorn", "--hint=string:sound-name:message-new-instant", "--hint=string:desktop-entry:popcorn", "Popcorn asks", &body])
                .status()
                .ok();
        });
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
        if self.settings && k.key == "escape" {
            self.settings = false;
            self.confirm_erase = false;
            cx.notify();
            return;
        }
        if self.closing && k.key == "escape" {
            self.closing = false;
            cx.notify();
            return;
        }
        if self.refreshing.is_some() && k.key == "escape" {
            self.refreshing = None;
            cx.notify();
            return;
        }
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
                            "This stops the corn. It erases the disk, the activity and the exported files. Copies in other folders do not change."
                        } else {
                            "This stops the corn. It erases the disk and the activity. Files on your computer do not change."
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

    /// Closing Popcorn stops every corn. With a terminal open, ask first; true means close now.
    fn may_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.cases.iter().any(Case::terminal_open) {
            return true;
        }
        self.closing = true;
        cx.notify();
        false
    }

    fn close_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.closing {
            return None;
        }
        let t = theme();
        let open: Vec<&str> = self.cases.iter().filter(|c| c.terminal_open()).map(|c| c.name.as_str()).collect();
        let text = match open.as_slice() {
            [] => "Closing Popcorn stops every corn.".to_string(),
            [one] => format!("A terminal is open in {one}. Closing Popcorn stops every corn, and the terminal disconnects."),
            many => format!("Terminals are open in {}. Closing Popcorn stops every corn, and the terminals disconnect.", many.join(", ")),
        };
        Some(
            div()
                .id("close-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.32))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.closing = false;
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
                        .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).child("Close Popcorn?"))
                        .child(div().text_color(t.muted).line_height(px(21.)).child(text))
                        .child(
                            div()
                                .pt_3()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(button("cancel-close", None, "Cancel", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.closing = false;
                                    cx.notify();
                                })))
                                .child(button("confirm-close", Some("square-stop"), "Close and stop corns", Look::Danger).on_click(|_, window, _| window.remove_window())),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Refresh copy deletes the corn's copy first, so it asks.
    fn refresh_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme();
        let host = self.refreshing.as_ref()?;
        let name = host.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Some(
            div()
                .id("refresh-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.32))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.refreshing = None;
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
                        .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).child(format!("Replace ~/work/{name}?")))
                        .child(div().text_color(t.muted).line_height(px(21.)).child("The corn's copy gets your folder again. Changes in the corn that you did not approve are lost. Review changes first to keep them."))
                        .child(
                            div()
                                .pt_3()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(button("cancel-refresh", None, "Cancel", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.refreshing = None;
                                    cx.notify();
                                })))
                                .child(button("review-first", Some("check"), "Review changes", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.refreshing = None;
                                    this.review_changes(cx);
                                    cx.notify();
                                })))
                                .child(button("confirm-refresh", None, "Replace copy", Look::Danger).on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(host) = this.refreshing.take() {
                                        this.send_folder(host, cx);
                                    }
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

    fn on_filter_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let k = &e.keystroke;
        match k.key.as_str() {
            "escape" => {
                self.filter.clear();
                window.focus(&self.focus);
            }
            "enter" => window.focus(&self.focus),
            "backspace" => {
                self.filter.pop();
            }
            _ if k.modifiers.secondary() && k.key == "v" => {
                if let Some(p) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.filter.push_str(p.lines().next().unwrap_or(""));
                }
            }
            _ => {
                if let Some(c) = k.key_char.as_deref().filter(|c| !k.modifiers.secondary() && !c.chars().any(char::is_control)) {
                    self.filter.push_str(c);
                }
            }
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
                        "Popcorn is a workbench for risky work. Use it for untrusted code, suspicious packages and unknown binaries. You and Claude do the work in a corn. Your computer stays isolated.",
                    ))
                    .child(block(
                        div().text_size(px(62.)).line_height(px(72.)).child("🌽").into_any_element(),
                        "Corn",
                        "A corn is a small virtual machine with its own Linux kernel. It cannot see your files, SSH keys or network. Each connection waits for your approval. Your Claude token stays outside the corn. Files come back only after you approve them. When you finish, delete the corn.",
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
                    .child("No token? Run this command in a terminal:")
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

    /// Start over: stop and delete every corn, and forget your name, token and settings.
    /// The workbench image stays, because it holds nothing personal and takes minutes to build.
    fn erase_all(&mut self) {
        for c in self.cases.drain(..) {
            c.delete();
        }
        if let Some(cfg) = std::env::home_dir().map(|h| h.join(".config/popcorn")) {
            std::fs::remove_dir_all(cfg).ok();
        }
        std::fs::remove_dir_all(sandbox::corns()).ok();
        gate::forget_all();
        self.selected = 0;
        self.has_name = false;
        self.has_key = gate::credential().is_some();
        self.claude_skipped = false;
        self.intro_seen = false;
        self.name.clear();
        self.token.clear();
        self.review = None;
        self.appearance = Appearance::load();
        self.settings = false;
        self.confirm_erase = false;
        self.notice = None;
    }

    /// App settings, one section per topic. Add new sections here.
    fn settings_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings {
            return None;
        }
        let t = theme();
        let current = self.appearance;
        let section = |title: &str| div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(t.muted).child(title.to_string());
        let mode = |a: Appearance, label: &'static str, icon: &'static str| {
            let on = a == current;
            div()
                .id(label)
                .flex_1()
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .gap_1p5()
                .rounded(px(5.))
                .cursor_pointer()
                .text_size(px(13.))
                .when(on, |d| d.bg(t.bg).font_weight(FontWeight::MEDIUM).border_1().border_color(t.seam))
                .when(!on, |d| d.text_color(t.muted).hover(|s| s.text_color(t.fg)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.appearance = a;
                    a.save();
                    this.apply_appearance(window);
                    cx.notify();
                }))
                .child(ui_icon(icon).size(px(14.)).text_color(if on { t.fg } else { t.muted }))
                .child(label)
        };
        let claude = match gate::credential_hint() {
            Some(hint) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(ui_icon("claude-code").size(px(16.)).text_color(t.muted))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().font_weight(FontWeight::MEDIUM).child("Connected")).child(div().text_size(px(12.5)).text_color(t.muted).font_family(CODE_FONT).truncate().child(hint)))
                .child(button("disconnect", None, "Disconnect", Look::Plain).text_color(t.bad).on_click(cx.listener(|this, _, _, cx| {
                    this.notice = match gate::remove_credential() {
                        Ok(()) => {
                            this.has_key = false;
                            // Setup stays done: you can connect again from here or from a corn.
                            this.claude_skipped = true;
                            if let Some(p) = std::env::home_dir().map(|h| h.join(".config/popcorn/claude-skipped")) {
                                std::fs::write(p, "").ok();
                            }
                            None
                        }
                        Err(e) => Some((e, false)),
                    };
                    cx.notify();
                })))
                .into_any_element(),
            None => div()
                .flex()
                .items_center()
                .gap_3()
                .child(ui_icon("claude-code").size(px(16.)).text_color(t.muted))
                .child(div().flex_1().flex().items_center().gap_1p5().child(div().font_weight(FontWeight::MEDIUM).child("Not connected")).child(ui_icon("unplug").size(px(13.)).text_color(t.bad)))
                .child(button("settings-connect", None, "Connect", Look::Plain).on_click(cx.listener(|this, _, window, cx| {
                    this.settings = false;
                    this.connecting = true;
                    window.focus(&this.token_focus);
                    cx.notify();
                })))
                .into_any_element(),
        };
        Some(
            div()
                .id("settings-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.32))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.settings = false;
                    this.confirm_erase = false;
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(460.))
                        .p_6()
                        .flex()
                        .flex_col()
                        .gap_5()
                        .bg(t.bg)
                        .rounded(px(10.))
                        .border_1()
                        .border_color(t.seam)
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_size(px(19.)).font_weight(FontWeight::SEMIBOLD).child("Settings"))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(section("Appearance"))
                                .child(div().p(px(2.)).flex().gap_0p5().rounded(px(7.)).bg(t.surface).child(mode(Appearance::Light, "Light", "sun")).child(mode(Appearance::Dark, "Dark", "moon")).child(mode(Appearance::System, "System", "monitor"))),
                        )
                        .child(div().h(px(1.)).bg(t.seam))
                        .child(div().flex().flex_col().gap_2().child(section("Claude")).child(claude))
                        .child(div().h(px(1.)).bg(t.seam))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(section("Data"))
                                .child(div().text_size(px(12.5)).text_color(t.muted).line_height(px(18.)).child(if self.confirm_erase {
                                            "This deletes all corns, your name, your Claude token and your settings. You cannot undo this."
                                        } else {
                                            "Delete all corns and settings. The workbench image stays."
                                        }))
                                .child(div().flex().gap_2().when(self.confirm_erase, |d| {
                                            d.child(button("cancel-erase", None, "Cancel", Look::Quiet).on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_erase = false;
                                                cx.notify();
                                            })))
                                        })
                                        .child(button("erase", None, if self.confirm_erase { "Erase all" } else { "Erase Popcorn data" }, if self.confirm_erase { Look::Danger } else { Look::Plain }).when(!self.confirm_erase, |d| d.text_color(t.bad)).on_click(cx.listener(|this, _, window, cx| {
                                            if this.confirm_erase {
                                                this.erase_all();
                                                this.apply_appearance(window);
                                                window.focus(&this.focus);
                                            } else {
                                                this.confirm_erase = true;
                                            }
                                            cx.notify();
                                        })))),
                        )
                        .when_some(self.notice.clone().filter(|(_, ok)| !ok), |d, (msg, _)| d.child(div().text_size(px(13.)).text_color(t.bad).child(msg)))
                        .child(div().flex().justify_end().child(button("settings-close", None, "Close", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                            this.settings = false;
                            cx.notify();
                        })))),
                )
                .into_any_element(),
        )
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
                        .child(div().text_color(t.muted).line_height(px(21.)).child("The token stays on this computer. Popcorn adds it only to requests to api.anthropic.com."))
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
            Err(_) if self.token.is_empty() => self.notice = Some(("Paste the token first. A Claude token starts with sk-ant-.".into(), false)),
            Err(_) => self.notice = Some((format!("A Claude token starts with sk-ant-. This one starts with \"{}\".", self.token.chars().take(6).collect::<String>()), false)),
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
        // Files only: the system dialog in folder mode cannot select files. Drop a folder on the window to copy it.
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Copy in".into()) });
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
        self.notice = Some(("Popcorn copies the folder into the corn…".into(), true));
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { share::send(&dir, &log, &path) }).await;
            this.update(cx, |this, cx| {
                this.notice = Some(match r {
                    Ok(n) => (format!("Copied {n} files. Popcorn did not copy .git and ignored files."), true),
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
        let (dir, log) = (c.dir.clone(), c.log.clone());
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { share::pull(&dir, &log) }).await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(changes) if changes.is_empty() => this.notice = Some(("There are no changes.".into(), true)),
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
                    Ok((_, 0)) => ("The outbox is empty. Save results in ~/outbox. Then export again.".into(), false),
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

    /// Release builds download the image; development builds make it with Docker.
    fn get_image(&mut self, cx: &mut Context<Self>) {
        if sandbox::IMAGE_SHA256.is_none() {
            return self.build_image(cx);
        }
        self.build = Build::Running("Downloading…".into());
        let got = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let total = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (g, all, flag) = (got.clone(), total.clone(), done.clone());
        cx.spawn(async move |this, cx| {
            let task = cx.background_executor().spawn(async move {
                let _set = Finish(flag);
                sandbox::download_image(&g, &all)
            });
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let (g, all) = (got.load(std::sync::atomic::Ordering::Relaxed), total.load(std::sync::atomic::Ordering::Relaxed));
                let text = match (g, all) {
                    (u64::MAX, _) => "Checking and unpacking…".to_string(),
                    (0, _) => "Downloading…".into(),
                    (g, 0) => format!("Downloading… {} MB", g >> 20),
                    (g, all) => format!("Downloading… {}%  ({} of {} MB)", g * 100 / all, g >> 20, all >> 20),
                };
                this.update(cx, |this, cx| {
                    this.build = Build::Running(text);
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
                if status.success() { Ok(()) } else { Err("The image build stopped. Read build.log for the cause.".to_string()) }
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
            .child(div().flex().items_center().gap_0p5().child(control_like("settings", "settings", cx.listener(|this, _, window, cx| {
                this.settings = true;
                this.notice = None;
                window.focus(&this.focus);
                cx.notify();
            }))).when(!mac, |d| {
                d.when(controls.minimize, |d| d.child(control("min", "minus", WindowControlArea::Min, false, |w, _| w.minimize_window())))
                    .when(controls.maximize, |d| d.child(control("max", if maximized { "copy" } else { "square" }, WindowControlArea::Max, false, |w, _| w.zoom_window())))
                    .child(control("close", "x", WindowControlArea::Close, true, {
                    let v = cx.entity().downgrade();
                    move |w, cx| {
                        if v.update(cx, |v, cx| v.may_close(cx)).unwrap_or(true) {
                            w.remove_window();
                        }
                    }
                }))
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
                .child(ui_icon(if sandbox::IMAGE_SHA256.is_some() { "cloud-download" } else { "hammer" }).size(px(15.)).text_color(t.muted))
                .child(div().font_family(CODE_FONT).truncate().child(if line.is_empty() { "Building…".to_string() } else { line.clone() }))
                .into_any_element(),
            Build::Failed(e) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_color(t.bad).child(e.clone()))
                .child(button("build", Some("hammer"), "Try again", Look::Primary).on_click(cx.listener(|this, _, _, cx| this.get_image(cx))))
                .into_any_element(),
            Build::Idle if sandbox::IMAGE_SHA256.is_some() => button("build", Some("cloud-download"), "Download the image", Look::Primary).on_click(cx.listener(|this, _, _, cx| this.get_image(cx))).into_any_element(),
            Build::Idle => button("build", Some("hammer"), "Build the image", Look::Primary).on_click(cx.listener(|this, _, _, cx| this.get_image(cx))).into_any_element(),
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
                                .child(div().text_size(px(62.)).line_height(px(72.)).mb_2().child("🍿"))
                                .child(div().text_size(px(30.)).font_weight(FontWeight::SEMIBOLD).line_height(px(36.)).child("Set up your workbench"))
                                .child(div().text_size(px(15.)).text_color(t.muted).line_height(px(23.)).child("Do these three steps one time. Each corn is a microVM with its own kernel. A corn cannot see your files, SSH keys or network.")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(step(1, self.has_name, "Your name", ""))
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
                                .child(if sandbox::IMAGE_SHA256.is_some() {
                                    step(2, image, "Download the workbench image", "A Debian image with Node, Python, git, curl, strace and Claude Code. All corns use it. Popcorn checks the download against this release before it uses it.")
                                } else {
                                    step(2, image, "Build the workbench image", "Popcorn uses Docker to build a Debian image. It has Node, Python, git, curl, strace and Claude Code. All corns use it. The build takes a few minutes.")
                                })
                                .when(!image, |d| d.child(div().pl(px(42.)).flex().child(build))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(step(3, self.has_key, "Connect Claude", "The token stays on this computer. Popcorn adds it only to requests to api.anthropic.com."))
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
                    .child(div().text_size(px(62.)).line_height(px(72.)).mb_3().child("🌽"))
                    .child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).child("Create your first corn"))
                    .child(div().text_size(px(15.)).text_color(t.muted).line_height(px(23.)).child("A corn is a machine for one risky job. Examples are untrusted code and suspicious downloads. All that runs in a corn stays in the corn."))
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
                                .child(div().text_size(px(13.)).child("This corn tries to connect to"))
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
                                        .child(format!("The connection waits for you. Popcorn refuses it in {left}s.{}", if more > 0 { format!(" {more} more requests wait.") } else { String::new() })),
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
                        .when(c.host_moved, |d| d.child(div().flex_shrink_0().text_size(px(12.)).text_color(t.signal).child("You also changed this file")))
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
                            .when(diff.is_empty(), |d| d.child(div().text_color(t.muted).child("This file is binary or empty."))),
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
                        .child(button("done-review", None, "Close", Look::Quiet).tooltip(|_, cx| cx.new(|_| Tip("Leave the rest undecided. Review again at any time.")).into()).on_click(cx.listener(|this, _, _, cx| {
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
                .child(div().pb_3().text_size(px(13.)).text_color(t.muted).child("Popcorn writes only the changes that you approve."))
                .children(rows)
                .into_any_element(),
        )
    }

    fn timeline(&self, events: &[Event], running: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let needle = self.filter.to_lowercase();
        let since = if self.span == 0 { 0 } else { now().saturating_sub(self.span) };
        // The verdict word counts too, so "refused" finds every refused connection.
        let verdict = |k: Kind| match k {
            Kind::Blocked => "refused",
            Kind::Allowed => "allowed",
            Kind::Claude => "claude",
            Kind::Note => "",
        };
        let shown: Vec<&Event> = events
            .iter()
            .filter(|e| e.at >= since && (needle.is_empty() || e.text.to_lowercase().contains(&needle) || verdict(e.kind).contains(needle.as_str())))
            .collect();
        let span = |label: &'static str, secs: u64| {
            let on = self.span == secs;
            div()
                .id(label)
                .px_1p5()
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(12.))
                .when(on, |d| d.bg(t.surface).text_color(t.fg))
                .when(!on, |d| d.text_color(t.faint).hover(|s| s.text_color(t.fg)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.span = secs;
                    cx.notify();
                }))
                .child(label)
        };
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
                    .gap_3()
                    .child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("What happened"))
                    .child(div().flex().gap_0p5().child(span("All", 0)).child(span("1h", 3600)).child(span("15m", 900)))
                    .child(
                        div()
                            .id("filter")
                            .track_focus(&self.filter_focus)
                            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_filter_key(e, window, cx)))
                            .w(px(180.))
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .rounded(px(5.))
                            .bg(t.surface)
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .focus(|s| s.border_color(t.fg))
                            .text_size(px(12.5))
                            .cursor_text()
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.filter_focus);
                                cx.notify();
                            }))
                            .child(ui_icon("search").size(px(13.)).text_color(t.faint))
                            .child(div().flex_1().min_w_0().truncate().when(self.filter.is_empty(), |d| d.text_color(t.faint)).child(if self.filter.is_empty() { "Filter".to_string() } else { self.filter.clone() })),
                    ),
            )
            .when(shown.is_empty() && !events.is_empty(), |d| d.child(div().py_8().text_color(t.muted).child("Nothing matches.")))
            .when(events.is_empty(), |d| {
                d.child(
                    div()
                        .py_8()
                        .text_color(t.muted)
                        .line_height(px(22.))
                        .child(if running { "No traffic yet. Open a terminal, drop files on this window, then start work." } else { "Start the corn. Popcorn shows all of its traffic here." }),
                )
            })
            // The same event in a row shows once, with a count: Claude Code makes many identical calls.
            .children(stacked_refs(&shown).into_iter().take(300).map(|(e, n)| {
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
                    .when(n > 1, |d| d.child(div().flex_shrink_0().font_family(CODE_FONT).text_size(px(12.)).text_color(t.faint).child(format!("×{n}"))))
            }))
            .into_any_element()
    }

    /// The boundary between this computer and the case, lane by lane, with what crossed it.
    fn airlock(&self, c: &Case, x: &Crossings, cx: &mut Context<Self>) -> AnyElement {
        let t = theme();
        let sealed = c.mode == Mode::Sealed;
        let mine: Vec<String> = gate::allowed().lock().unwrap().iter().cloned().collect();
        let lane_with = |icon: &'static str, tint: Hsla, name: &str, badge: Option<AnyElement>, rule: &str, count: usize, color: Hsla| {
            div()
                .py_3()
                .flex()
                .items_start()
                .gap_3()
                .child(div().h(px(22.)).flex().items_center().child(ui_icon(icon).size(px(16.)).text_color(tint)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().h(px(22.)).flex().items_center().gap_1p5().child(div().font_weight(FontWeight::MEDIUM).child(name.to_string())).children(badge))
                        .child(div().text_size(px(12.5)).text_color(t.muted).line_height(px(18.)).child(rule.to_string())),
                )
                .child(div().h(px(22.)).flex().items_center().font_family(CODE_FONT).text_size(px(15.)).font_weight(FontWeight::MEDIUM).text_color(if count > 0 { color } else { t.faint }).child(count.to_string()))
        };
        let lane = |icon: &'static str, name: &str, rule: &str, count: usize, color: Hsla| lane_with(icon, t.muted, name, None, rule, count, color);
        // Disconnected: a small red mark after the name. Click it to connect. Connected: nothing.
        let unplugged = (!self.has_key).then(|| {
            div()
                .id("claude-unplugged")
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(t.bad.opacity(0.14)))
                .tooltip(|_, cx| cx.new(|_| Tip("Claude is not connected. Click to connect.")).into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.connecting = true;
                    this.notice = None;
                    window.focus(&this.token_focus);
                    cx.notify();
                }))
                .child(ui_icon("unplug").size(px(13.)).text_color(t.bad))
                .into_any_element()
        });
        let network = |m: Mode, label: &'static str| {
            let on = c.mode == m;
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
        div()
            .flex()
            .flex_col()
            .child(div().pb_2().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Network"))
            .child(div().pb_8().flex().child(div().p(px(2.)).flex().gap_0p5().rounded(px(7.)).bg(t.surface).child(network(Mode::Ask, "Ask before connecting")).child(network(Mode::Sealed, "Sealed"))))
            .child(div().pb_2().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Shared folder"))
            .child(match share::get(&c.dir) {
                None => div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(px(13.)).text_color(t.faint).child("The corn gets a copy. Changes come back only after you approve them."))
                    .child(div().flex().child(button("share", Some("folder-lock"), "Share a folder", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                        if this.case().is_some_and(|c| c.running()) {
                            this.share_folder(cx);
                        } else {
                            this.notice = Some(("Start the corn. Then share a folder.".into(), false));
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
                        .child(div().font_family(CODE_FONT).text_size(px(13.)).truncate().child(format!("~/work/{}", sh.name)))
                        .child(div().text_size(px(12.)).text_color(t.faint).truncate().child(format!("From {}", sh.host.display())))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(button("review", Some("check"), "Review changes", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                                    this.review_changes(cx);
                                    cx.notify();
                                })))
                                .child(
                                    icon_button("resend", "refresh-cw", t.muted, "Refresh copy")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.refreshing = Some(host.clone());
                                            window.focus(&this.focus);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    icon_button("unshare", "square-stop", t.bad.opacity(0.75), "Stop sharing")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(c) = this.cases.get(this.selected) {
                                                share::stop(&c.dir, &c.log);
                                            }
                                            this.review = this.review.take().filter(|r| r.corn != this.selected);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .into_any_element()
                }
            })
            .child(div().pt_8().pb_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("The airlock"))
            .child(div().pb_3().text_size(px(13.)).text_color(t.muted).child("All traffic between this computer and the corn goes through here."))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(lane_with(
                        "claude-code",
                        t.muted,
                        "Claude",
                        unplugged,
                        if self.has_key { "Claude Code connects to Anthropic. Popcorn adds your token outside the corn." } else { "Not connected. Click the red icon to connect." },
                        x.claude,
                        t.fg,
                    ))
                    .child(lane("network", "Network", if sealed { "Popcorn refuses and records each connection." } else { "Each new host waits for your approval." }, x.allowed + x.refused, if x.refused > 0 { t.bad } else { t.ok }))
                    .child(lane("upload", "Files in", "Files that you drop or copy in go to ~/inbox.", x.files_in, t.fg))
                    .child(lane("folder-lock", "Files out", "Only files that you export from ~/outbox. They cannot run.", x.files_out, t.fg)),
            )
            .child(div().pt_6().pb_2().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Always allowed"))
            .when(mine.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(t.faint).child("Hosts that you always allow show here. They apply to every corn.")))
            .children(mine.into_iter().enumerate().map(|(i, h)| {
                let host = h.clone();
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().font_family(CODE_FONT).text_size(px(13.)).child(h))
                    .child(button(("forget", i), None, "Remove", Look::Quiet).h(px(24.)).text_color(t.muted).on_click(cx.listener(move |_, _, _, cx| {
                        gate::forget(&host);
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
        let name = case.name.clone();
        let events: Vec<Event> = case.log.lock().unwrap().clone();
        let x = crossings(&events);
        let airlock = self.airlock(&self.cases[self.selected], &x, cx);
        let (left, right) = (self.left_scroll.clone(), self.right_scroll.clone());
        let timeline = match self.changes_view(cx) {
            Some(changes) => changes,
            None => self.timeline(&events, running, cx),
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
                            .group("title")
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).line_height(px(32.)).child(name))
                            .child(
                                div()
                                    .id("rename-pencil")
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .invisible()
                                    .group_hover("title", |s| s.visible())
                                    .hover(|s| s.bg(t.hover))
                                    .tooltip(|_, cx| cx.new(|_| Tip("Rename")).into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let name = this.cases.get(this.selected).map(|c| c.name.clone()).unwrap_or_default();
                                        this.renaming = Some((this.selected, name));
                                        window.focus(&this.rename_focus);
                                        cx.notify();
                                    }))
                                    .child(ui_icon("pencil").size(px(16.)).text_color(t.faint)),
                            )
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
                                        _ => "Stopped. Popcorn keeps the disk until you delete the corn.".into(),
                                    }),
                            )
                    ),
            )
            .when(running, |d| {
                d.child(button("copy-in", Some("upload"), "Copy in", Look::Plain).on_click(cx.listener(|this, _, _, cx| this.copy_in(cx))))
                .child(button("export", Some("package"), "Export outbox", Look::Plain).on_click(cx.listener(|this, _, _, cx| this.export(cx))))
                .child(button("term", Some("terminal"), "Open terminal", Look::Primary).on_click(cx.listener(|this, _, _, cx| {
                    let r = this.case().map(|c| c.open_terminal()).unwrap_or(Ok(()));
                    this.report(r);
                    cx.notify();
                })))
                .child(button_tinted("stop", Some("square-stop"), Some(t.bad.opacity(0.75)), "Stop", Look::Plain).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(c) = this.case() {
                        c.stop();
                    }
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
                    this.notice = Some(("Start the corn. Then drop the files again.".into(), false));
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_8()
                    .flex()
                    .gap_12()
                    .child(div().flex_1().min_w_0().h_full().child(scroll_column("case-left", &left, div().py_6().child(timeline).into_any_element(), cx)))
                    .child(div().w(px(330.)).flex_shrink_0().h_full().child(scroll_column("case-right", &right, div().py_6().pr(px(3.)).child(airlock).into_any_element(), cx))),
            )
            .into_any_element()
    }
}

/// A small text tooltip.
struct Tip(&'static str);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = theme();
        div().px_2().py_1().rounded(px(5.)).bg(t.fg).text_color(t.bg).text_size(px(12.)).font_family(UI_FONT).child(self.0)
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
            .children(self.refresh_dialog(cx))
            .children(self.close_dialog(cx))
            .children(self.settings_dialog(cx))
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
                // The window manager's close (Alt+F4, the dock) asks like the close button.
                let v = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| v.update(cx, |v, cx| v.may_close(cx)).unwrap_or(true));
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
