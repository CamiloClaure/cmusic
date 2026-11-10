use crate::app::{thumb, Message, State};
use crate::theme::Look;
use cmusic_core::{lyrics, Item, PageRequest, Repeat, Status, Track};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, image, row, scrollable, slider, space, stack, text, text_input,
};
use iced::{border, gradient, Background, Center, Color, ContentFit, Element, Fill, Theme};

/// Rows rendered per list.
// ponytail: no virtualization; a lazy/windowed list is the upgrade if 500 rows ever feels slow.
const MAX_ROWS: usize = 500;

pub fn view(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let content = column![
        row![
            panel(l, sidebar(s)),
            panel(l, main_area(s)).width(Fill),
            panel(l, queue(s))
        ]
        .spacing(l.gap)
        .height(Fill),
        panel(l, column![bar(s), controls(s)].spacing(l.pad)),
    ]
    .spacing(l.gap)
    .padding(l.gap);
    // Glass needs something to sit on: the blurred art of the current track, dimmed by a scrim.
    let backdrop: Element<_> = match &s.backdrop {
        Some((_, h)) if l.backdrop => image(h.clone())
            .content_fit(ContentFit::Cover)
            .width(Fill)
            .height(Fill)
            .into(),
        _ => space().into(),
    };
    stack![
        container(backdrop).width(Fill).height(Fill).style(base(l)),
        container(content).width(Fill).height(Fill).style(scrim(l)),
    ]
    .into()
}

fn panel<'a>(
    l: Look,
    content: impl Into<Element<'a, Message>>,
) -> iced::widget::Container<'a, Message> {
    container(content).padding(l.gap).style(glass(l))
}

/// Every text size goes through here (16 is iced's default text size).
fn sz(l: Look, n: f32) -> f32 {
    n * l.font_scale
}

fn txt<'a>(l: Look, t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(t).size(sz(l, 16.0))
}

fn line<'a>(l: Look, t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    txt(l, t).wrapping(Wrapping::None)
}

/// Rail width plus the gap an embedded scrollbar takes from its content.
const RAIL: f32 = 16.0;

/// Shortens `t` to fit `width` px at base text size `base` (iced has no ellipsis), cutting on a
/// char boundary and ending with "...".
// ponytail: width is estimated at 0.5 em per char; real text measurement is the upgrade.
fn fit(l: Look, t: &str, width: f32, base: f32) -> String {
    let budget = (width / (sz(l, base) * 0.5)).max(4.0) as usize;
    if t.chars().count() <= budget {
        return t.to_string();
    }
    let head: String = t.chars().take(budget - 3).collect();
    format!("{}...", head.trim_end())
}

fn art<'a>(s: &'a State, url: Option<&String>, size: u32) -> Element<'a, Message> {
    let l = s.look;
    match url.and_then(|u| s.art.get(u)) {
        Some(h) => image(h.clone())
            .width(size)
            .height(size)
            .content_fit(ContentFit::Cover)
            .border_radius(size as f32 / 5.0)
            .into(),
        None => container(space())
            .width(size)
            .height(size)
            .style(move |_| container::Style {
                background: Some(tint(l.control, 0.08).into()),
                border: border::rounded(8),
                ..Default::default()
            })
            .into(),
    }
}

fn sidebar(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let open = s.current.as_ref().map(|(r, _)| r);
    let nav = |label: &'static str, r: PageRequest| {
        let on = open == Some(&r);
        button(txt(l, label))
            .on_press(Message::Open(r))
            .width(Fill)
            .padding([8, 12])
            .style(row_button(l, on))
    };
    column![
        text_input("Search", &s.search)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .padding([8, 14])
            .size(sz(l, 16.0))
            .style(field(l)),
        nav("Liked songs", PageRequest::LikedSongs),
        nav("Playlists", PageRequest::LibraryPlaylists),
        nav("Songs", PageRequest::LibrarySongs),
        nav("Albums", PageRequest::LibraryAlbums),
        nav("Artists", PageRequest::LibraryArtists),
        space().height(Fill),
        button(
            txt(l, if s.logged_in { "Account" } else { "Log in" })
                .center()
                .width(Fill)
        )
        .on_press(Message::OpenLogin)
        .width(Fill)
        .style(pill(l, false)),
    ]
    .spacing(4)
    .width(180)
    .into()
}

fn main_area(s: &State) -> Element<'_, Message> {
    let l = s.look;
    if s.login.is_some() {
        return login(s);
    }
    if s.show_lyrics {
        return lyrics_view(s);
    }
    let title = match (&s.awaiting, &s.current) {
        (Some(_), _) => "Loading...".to_string(),
        (None, Some((_, p))) => p.title.clone(),
        (None, None) => "Search or pick something on the left".to_string(),
    };
    let header = row![
        text(title)
            .size(sz(l, 22.0))
            .wrapping(Wrapping::None)
            .width(Fill),
        button(txt(l, "Back"))
            .on_press_maybe((!s.history.is_empty()).then_some(Message::Back))
            .style(pill(l, false)),
    ]
    .align_y(Center);
    let mut rows = column![].spacing(4);
    // Window minus panels, sidebar, queue, art, duration, three buttons and the gaps between.
    let avail =
        (s.size.width - 10.0 * l.gap - 460.0 - 5.0 * l.pad - RAIL - 250.0 * l.font_scale).max(80.0);
    let mut note = None;
    if let Some((_, page)) = &s.current {
        for (i, it) in page.items.iter().enumerate().take(MAX_ROWS) {
            rows = rows.push(item_row(s, i, it, avail));
        }
        if page.items.len() > MAX_ROWS {
            note = Some(format!("showing {MAX_ROWS} of {}", page.items.len()));
        }
    }
    column![
        header,
        scrollable(rows.padding([0.0, l.pad]))
            .spacing(6)
            .style(scroll(l))
            .height(Fill)
            .width(Fill),
        text(note.unwrap_or_default()).size(sz(l, 13.0))
    ]
    .spacing(l.pad)
    .width(Fill)
    .into()
}

fn item_row<'a>(s: &'a State, i: usize, it: &'a Item, w: f32) -> Element<'a, Message> {
    let l = s.look;
    let (title, sub, open): (&str, &str, Option<PageRequest>) = match it {
        Item::Track(t) => (&t.title, &t.artists, None),
        Item::Playlist(e) => (
            &e.title,
            &e.subtitle,
            Some(PageRequest::Playlist(e.id.clone())),
        ),
        Item::Album(e) => (
            &e.title,
            &e.subtitle,
            Some(PageRequest::Album(e.id.clone())),
        ),
        Item::Artist(e) => (
            &e.title,
            &e.subtitle,
            Some(PageRequest::Artist(e.id.clone())),
        ),
    };
    let label = button(column![
        line(l, fit(l, title, w, 16.0)),
        line(l, fit(l, sub, w, 13.0))
            .size(sz(l, 13.0))
            .color(l.text_dim)
    ])
    .on_press(open.map_or(Message::PlayRow(i), Message::Open))
    .width(Fill)
    .clip(true)
    .style(row_button(l, false));
    let mut r = row![art(s, thumb(it).as_ref(), 40), label]
        .spacing(l.pad)
        .align_y(Center);
    if let Item::Track(t) = it {
        r = r.push(
            line(
                l,
                t.duration_secs.map(|d| mmss(d as f64)).unwrap_or_default(),
            )
            .width(48),
        );
        r = r.push(small(l, "Next", Message::PlayNext(t.clone())));
        r = r.push(small(l, "Add", Message::Enqueue(t.clone())));
        r = r.push(small(l, "Like", Message::Like(t.video_id.clone())));
    }
    r.into()
}

fn small(l: Look, label: &'static str, m: Message) -> Element<'static, Message> {
    button(text(label).size(sz(l, 13.0)))
        .on_press(m)
        .padding([4, 10])
        .style(pill(l, false))
        .into()
}

fn lyrics_view(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let body: Element<_> = match s.current_lyrics() {
        Some(ly) if !ly.lines.is_empty() => {
            let cur = lyrics::current_line(ly, s.snapshot.position_secs);
            let mut c = column![].spacing(6);
            for (i, (_, t)) in ly.lines.iter().enumerate() {
                let t = text(t.as_str()).size(sz(l, 18.0));
                c = c.push(if Some(i) == cur {
                    t.size(sz(l, 21.0)).color(tint(l.text, 1.0))
                } else {
                    t.color(tint(l.text_dim, 0.45))
                });
            }
            // ponytail: no auto-scroll to the current line; add scrollable::scroll_to if wanted.
            scrollable(c.padding(l.pad))
                .spacing(6)
                .style(scroll(l))
                .height(Fill)
                .width(Fill)
                .into()
        }
        _ => txt(l, "No lyrics").into(),
    };
    column![text("Lyrics").size(sz(l, 22.0)), body]
        .spacing(l.pad)
        .width(Fill)
        .into()
}

fn login(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let lg = s.login.as_ref().expect("checked by caller");
    column![
        text("Log in").size(sz(l, 22.0)),
        txt(
            l,
            lg.message
                .as_deref()
                .unwrap_or("Import your YouTube Music cookies.")
        ),
        button(txt(l, "Import from browser"))
            .on_press(Message::ImportBrowsers)
            .style(pill(l, true)),
        txt(l, "Or paste a Cookie header:"),
        text_input("Cookie: ...", &lg.cookie)
            .secure(true)
            .on_input(Message::CookieInput)
            .on_submit(Message::UseCookie)
            .padding([8, 14])
            .size(sz(l, 16.0))
            .style(field(l)),
        button(txt(l, "Use pasted cookie"))
            .on_press(Message::UseCookie)
            .style(pill(l, false)),
        button(txt(l, "Close"))
            .on_press(Message::CloseLogin)
            .style(pill(l, false)),
    ]
    .spacing(l.pad)
    .width(Fill)
    .into()
}

fn queue(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let snap = &s.snapshot;
    let mut c = column![].spacing(2);
    // Queue column minus the remove button, the row button padding and the rail.
    let w = 280.0 - 28.0 - 10.0 - RAIL;
    for (i, t) in snap.queue.iter().enumerate().take(MAX_ROWS) {
        let cur = snap.cursor == Some(i);
        let name = button(column![
            line(l, fit(l, &t.title, w, 16.0)),
            line(l, fit(l, &t.artists, w, 12.0)).size(sz(l, 12.0))
        ])
        .on_press(Message::Jump(i))
        .width(Fill)
        .clip(true)
        .style(row_button(l, false));
        let x = button(txt(l, "x").center())
            .on_press(Message::Remove(i))
            .width(28)
            .height(28)
            .padding(0)
            .style(row_button(l, false));
        c = c.push(
            container(row![name, x].align_y(Center))
                .width(Fill)
                .style(move |_| container::Style {
                    background: cur.then(|| tint(l.control, 0.14 + l.control.a).into()),
                    border: border::rounded(l.radius_row)
                        .color(if cur {
                            tint(l.accent_a, 0.7)
                        } else {
                            Color::TRANSPARENT
                        })
                        .width(1),
                    ..Default::default()
                }),
        );
    }
    column![
        row![
            text("Queue").size(sz(l, 22.0)).width(Fill),
            button(txt(l, "Clear"))
                .on_press_maybe((!snap.queue.is_empty()).then_some(Message::ClearQueue))
                .style(pill(l, false)),
        ]
        .align_y(Center),
        scrollable(c)
            .spacing(6)
            .style(scroll(l))
            .height(Fill)
            .width(Fill),
    ]
    .spacing(l.pad)
    .width(280)
    .into()
}

fn bar(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let snap = &s.snapshot;
    let cur: Option<&Track> = snap.current();
    let status = match snap.status {
        Status::Loading => "Loading...",
        Status::Buffering => "Buffering...",
        Status::Error => "Error",
        _ => "",
    };
    let info = column![
        line(
            l,
            fit(
                l,
                cur.map_or("Nothing playing", |t| t.title.as_str()),
                220.0,
                16.0
            )
        ),
        line(
            l,
            fit(
                l,
                match (status, cur) {
                    ("", Some(t)) => t.artists.as_str(),
                    ("", None) => "",
                    (st, _) => st,
                },
                220.0,
                13.0
            )
        )
        .size(sz(l, 13.0))
        .color(l.text_dim),
    ];
    let info = container(info).width(220).clip(true);
    let playing = matches!(snap.status, Status::Playing | Status::Buffering);
    let max = snap.duration_secs.max(1.0);
    let pos = s.drag.unwrap_or(snap.position_secs).min(max);
    row![
        art(s, cur.and_then(|t| t.thumbnail_url.as_ref()), 48),
        info,
        button(txt(l, "Prev"))
            .on_press(Message::Prev)
            .style(pill(l, false)),
        button(
            txt(l, if playing { "Pause" } else { "Play" })
                .center()
                .width(Fill)
        )
        .on_press(Message::TogglePause)
        .width(84)
        .padding([9, 0])
        .style(pill(l, true)),
        button(txt(l, "Next"))
            .on_press(Message::Next)
            .style(pill(l, false)),
        text(mmss(pos)).size(sz(l, 13.0)).color(l.text_dim),
        slider(0.0..=max, pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .width(Fill)
            .style(rail(l)),
        text(mmss(snap.duration_secs))
            .size(sz(l, 13.0))
            .color(l.text_dim),
    ]
    .spacing(l.pad)
    .align_y(Center)
    .into()
}

/// Second bar row: status line on the left, volume and toggles on the right.
fn controls(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let snap = &s.snapshot;
    let on = |label: String, active: bool, m: Message| {
        button(text(label).size(sz(l, 13.0)))
            .on_press(m)
            .padding([4, 10])
            .style(pill(l, active))
    };
    let repeat = match snap.repeat {
        Repeat::Off => "Repeat: off",
        Repeat::All => "Repeat: all",
        Repeat::One => "Repeat: one",
    };
    row![
        line(
            l,
            s.notice
                .as_deref()
                .or(snap.last_error.as_deref())
                .unwrap_or("")
        )
        .size(sz(l, 13.0))
        .color(l.text_dim)
        .width(Fill),
        text("Vol").size(sz(l, 13.0)),
        slider(0..=100u8, snap.volume, Message::Volume)
            .width(100)
            .style(rail(l)),
        on("Shuffle".into(), snap.shuffle, Message::Shuffle),
        on(repeat.into(), snap.repeat != Repeat::Off, Message::Repeat),
        on("Normalize".into(), snap.normalize, Message::Normalize),
        on("Lyrics".into(), s.show_lyrics, Message::ToggleLyrics),
        on(
            format!("Theme: {}", s.theme_name),
            false,
            Message::CycleTheme
        ),
    ]
    .spacing(l.pad)
    .align_y(Center)
    .into()
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

// Look: frosted glass over a blurred backdrop. iced has no backdrop filter, so the blur
// lives in the backdrop image and the panels are translucent sheets with a light edge.

/// `c` with its alpha replaced.
fn tint(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// A gradient, or a plain fill when every stop is the same colour (cheaper to render).
fn fill(angle: f32, stops: &[(f32, Color)]) -> Background {
    if stops.windows(2).all(|w| w[0].1 == w[1].1) {
        return stops[0].1.into();
    }
    let mut g = gradient::Linear::new(angle);
    for &(at, c) in stops {
        g = g.add_stop(at, c);
    }
    g.into()
}

fn accent(l: Look) -> Background {
    fill(
        std::f32::consts::FRAC_PI_2,
        &[(0.0, l.accent_a), (1.0, l.accent_b)],
    )
}

pub fn theme(l: Look) -> Theme {
    Theme::custom(
        "cmusic",
        iced::theme::Palette {
            background: l.ink,
            text: l.text,
            primary: l.accent_a,
            ..Theme::Dark.palette()
        },
    )
}

/// Shown behind the backdrop image, and alone before any art has loaded.
fn base(l: Look) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(fill(
            2.4,
            &[(0.0, l.base[0]), (0.55, l.base[1]), (1.0, l.base[2])],
        )),
        ..Default::default()
    }
}

/// Dims the backdrop so text stays readable on bright covers.
fn scrim(l: Look) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(fill(
            std::f32::consts::PI,
            &[(0.0, l.scrim_top), (1.0, l.scrim_bottom)],
        )),
        ..Default::default()
    }
}

fn glass(l: Look) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(l.text),
        // Brighter at the top edge, like light catching a pane.
        background: Some(fill(
            std::f32::consts::PI,
            &[
                (0.0, l.panel_top),
                (0.12, l.panel_mid),
                (1.0, l.panel_bottom),
            ],
        )),
        border: border::rounded(l.radius_panel)
            .color(l.panel_border)
            .width(1),
        // No drop shadow: without a GPU driver iced renders in software, where a large
        // blurred shadow per panel is the most expensive thing on screen.
        ..Default::default()
    }
}

/// Capsule button. `on` fills it with the accent (primary action, active toggle).
fn pill(l: Look, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let lift = match status {
            button::Status::Hovered => 0.08,
            button::Status::Pressed => 0.14,
            _ => 0.0,
        };
        let disabled = status == button::Status::Disabled;
        button::Style {
            background: Some(if on {
                accent(l)
            } else {
                tint(l.control, l.control.a + lift).into()
            }),
            text_color: match (on, disabled) {
                (_, true) => tint(l.text, 0.3),
                (true, _) => l.ink,
                _ => l.text,
            },
            border: border::rounded(999)
                .color(if on {
                    tint(Color::WHITE, 0.5 + lift)
                } else {
                    tint(l.control_border, l.control_border.a + lift)
                })
                .width(1),
            ..Default::default()
        }
    }
}

/// List row: invisible until hovered. `on` marks the current queue entry.
fn row_button(l: Look, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let a = match status {
            button::Status::Hovered => 0.10,
            button::Status::Pressed => 0.16,
            _ => 0.0,
        } + if on { 0.14 } else { 0.0 };
        button::Style {
            background: (a > 0.0).then(|| tint(l.control, a).into()),
            text_color: l.text,
            border: border::rounded(l.radius_row)
                .color(if on {
                    tint(l.accent_a, 0.7)
                } else {
                    Color::TRANSPARENT
                })
                .width(1),
            ..Default::default()
        }
    }
}

fn field(l: Look) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_, status| {
        let focused = matches!(status, text_input::Status::Focused { .. });
        text_input::Style {
            // An input: darker than the rows around it, with a clear edge.
            background: tint(l.ink, if focused { 0.45 } else { 0.35 }).into(),
            border: border::rounded(999)
                .color(if focused {
                    l.accent_a
                } else {
                    tint(l.text, 0.4)
                })
                .width(1),
            icon: tint(l.text, 0.6),
            placeholder: tint(l.text, 0.45),
            value: tint(l.text, 0.95),
            selection: tint(l.accent_a, 0.4),
        }
    }
}

fn rail(l: Look) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, status| slider::Style {
        rail: slider::Rail {
            backgrounds: (accent(l), tint(l.control, 0.16).into()),
            width: 4.0,
            border: border::rounded(2),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle {
                radius: if status == slider::Status::Active {
                    6.0
                } else {
                    8.0
                },
            },
            background: tint(l.text, 1.0).into(),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

fn scroll(l: Look) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
    move |theme, status| {
        let mut st = scrollable::default(theme, status);
        let rail = scrollable::Rail {
            background: None,
            border: border::rounded(4),
            scroller: scrollable::Scroller {
                background: l.panel_border.into(),
                border: border::rounded(4),
            },
        };
        st.vertical_rail = rail;
        st.horizontal_rail = rail;
        st
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cuts_on_char_boundaries() {
        let l = Look::default();
        // 96 px at 16 px text is a 12 char budget.
        assert_eq!(fit(l, "short", 96.0, 16.0), "short");
        assert_eq!(fit(l, "123456789012", 96.0, 16.0), "123456789012");
        assert_eq!(fit(l, "1234567890123", 96.0, 16.0), "123456789...");
        let jp = "ñandú canción mañana".to_string();
        let out = fit(l, &jp, 96.0, 16.0);
        assert_eq!(out.chars().count(), 12);
        assert!(out.ends_with("..."));
        assert_eq!(
            fit(l, "日本語日本語日本語日本語日本語", 96.0, 16.0),
            "日本語日本語日本語..."
        );
    }
}
