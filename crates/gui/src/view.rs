use crate::app::{thumb, Message, State};
use cmusic_core::{lyrics, Item, PageRequest, Repeat, Status, Track};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, image, row, scrollable, slider, space, stack, text, text_input,
};
use iced::{
    border, gradient, Background, Center, Color, ContentFit, Element, Fill, Shadow, Theme, Vector,
};

/// Rows rendered per list.
// ponytail: no virtualization; a lazy/windowed list is the upgrade if 500 rows ever feels slow.
const MAX_ROWS: usize = 500;
const PAD: f32 = 8.0;
const GAP: f32 = 14.0;

pub fn view(s: &State) -> Element<'_, Message> {
    let content = column![
        row![
            panel(sidebar(s)),
            panel(main_area(s)).width(Fill),
            panel(queue(s))
        ]
        .spacing(GAP)
        .height(Fill),
        panel(column![bar(s), controls(s)].spacing(PAD)),
    ]
    .spacing(GAP)
    .padding(GAP);
    // Glass needs something to sit on: the blurred art of the current track, dimmed by a scrim.
    let backdrop: Element<_> = match &s.backdrop {
        Some((_, h)) => image(h.clone())
            .content_fit(ContentFit::Cover)
            .width(Fill)
            .height(Fill)
            .into(),
        None => space().into(),
    };
    stack![
        container(backdrop).width(Fill).height(Fill).style(base),
        container(content).width(Fill).height(Fill).style(scrim),
    ]
    .into()
}

fn panel<'a>(content: impl Into<Element<'a, Message>>) -> iced::widget::Container<'a, Message> {
    container(content).padding(14).style(glass)
}

fn line<'a>(t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(t).wrapping(Wrapping::None)
}

fn art<'a>(s: &'a State, url: Option<&String>, size: u32) -> Element<'a, Message> {
    match url.and_then(|u| s.art.get(u)) {
        Some(h) => image(h.clone())
            .width(size)
            .height(size)
            .border_radius(size as f32 / 5.0)
            .into(),
        None => container(space())
            .width(size)
            .height(size)
            .style(|_| container::Style {
                background: Some(white(0.08).into()),
                border: border::rounded(8),
                ..Default::default()
            })
            .into(),
    }
}

fn sidebar(s: &State) -> Element<'_, Message> {
    let nav = |label: &'static str, r: PageRequest| {
        button(label)
            .on_press(Message::Open(r))
            .width(Fill)
            .padding([8, 12])
            .style(row_button(false))
    };
    column![
        text_input("Search", &s.search)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .padding([8, 14])
            .style(field),
        nav("Liked songs", PageRequest::LikedSongs),
        nav("Playlists", PageRequest::LibraryPlaylists),
        nav("Songs", PageRequest::LibrarySongs),
        nav("Albums", PageRequest::LibraryAlbums),
        nav("Artists", PageRequest::LibraryArtists),
        space().height(Fill),
        button(text("Log in").center().width(Fill))
            .on_press(Message::OpenLogin)
            .width(Fill)
            .style(pill(false)),
    ]
    .spacing(4)
    .width(180)
    .into()
}

fn main_area(s: &State) -> Element<'_, Message> {
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
        text(title).size(22).wrapping(Wrapping::None).width(Fill),
        button("Back")
            .on_press_maybe((!s.history.is_empty()).then_some(Message::Back))
            .style(pill(false)),
    ]
    .align_y(Center);
    let mut rows = column![].spacing(4);
    let mut note = None;
    if let Some((_, page)) = &s.current {
        for (i, it) in page.items.iter().enumerate().take(MAX_ROWS) {
            rows = rows.push(item_row(s, i, it));
        }
        if page.items.len() > MAX_ROWS {
            note = Some(format!("showing {MAX_ROWS} of {}", page.items.len()));
        }
    }
    column![
        header,
        scrollable(rows.padding([0.0, PAD]))
            .style(scroll)
            .height(Fill)
            .width(Fill),
        text(note.unwrap_or_default()).size(13)
    ]
    .spacing(PAD)
    .width(Fill)
    .into()
}

fn item_row<'a>(s: &'a State, i: usize, it: &'a Item) -> Element<'a, Message> {
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
        line(title),
        line(sub).size(13).style(text::secondary)
    ])
    .on_press(open.map_or(Message::PlayRow(i), Message::Open))
    .width(Fill)
    .clip(true)
    .style(row_button(false));
    let mut r = row![art(s, thumb(it).as_ref(), 40), label]
        .spacing(PAD)
        .align_y(Center);
    if let Item::Track(t) = it {
        r = r.push(line(t.duration_secs.map(|d| mmss(d as f64)).unwrap_or_default()).width(48));
        r = r.push(small("Next", Message::PlayNext(t.clone())));
        r = r.push(small("Add", Message::Enqueue(t.clone())));
        r = r.push(small("Like", Message::Like(t.video_id.clone())));
    }
    r.into()
}

fn small(label: &'static str, m: Message) -> Element<'static, Message> {
    button(text(label).size(13))
        .on_press(m)
        .padding([4, 10])
        .style(pill(false))
        .into()
}

fn lyrics_view(s: &State) -> Element<'_, Message> {
    let body: Element<_> = match s.current_lyrics() {
        Some(l) if !l.lines.is_empty() => {
            let cur = lyrics::current_line(l, s.snapshot.position_secs);
            let mut c = column![].spacing(6);
            for (i, (_, t)) in l.lines.iter().enumerate() {
                let t = text(t.as_str()).size(18);
                c = c.push(if Some(i) == cur {
                    t.size(21).color(Color::WHITE)
                } else {
                    t.color(white(0.45))
                });
            }
            // ponytail: no auto-scroll to the current line; add scrollable::scroll_to if wanted.
            scrollable(c.padding(PAD))
                .style(scroll)
                .height(Fill)
                .width(Fill)
                .into()
        }
        _ => text("No lyrics").into(),
    };
    column![text("Lyrics").size(22), body]
        .spacing(PAD)
        .width(Fill)
        .into()
}

fn login(s: &State) -> Element<'_, Message> {
    let l = s.login.as_ref().expect("checked by caller");
    column![
        text("Log in").size(22),
        text(
            l.message
                .as_deref()
                .unwrap_or("Import your YouTube Music cookies.")
        ),
        button("Import from browser")
            .on_press(Message::ImportBrowsers)
            .style(pill(true)),
        text("Or paste a Cookie header:"),
        text_input("Cookie: ...", &l.cookie)
            .secure(true)
            .on_input(Message::CookieInput)
            .on_submit(Message::UseCookie)
            .padding([8, 14])
            .style(field),
        button("Use pasted cookie")
            .on_press(Message::UseCookie)
            .style(pill(false)),
        button("Close")
            .on_press(Message::CloseLogin)
            .style(pill(false)),
    ]
    .spacing(PAD)
    .width(Fill)
    .into()
}

fn queue(s: &State) -> Element<'_, Message> {
    let snap = &s.snapshot;
    let mut c = column![].spacing(2);
    for (i, t) in snap.queue.iter().enumerate().take(MAX_ROWS) {
        let cur = snap.cursor == Some(i);
        let name = button(column![line(&t.title), line(&t.artists).size(12)])
            .on_press(Message::Jump(i))
            .width(Fill)
            .clip(true)
            .style(row_button(cur));
        c = c.push(
            row![
                name,
                button("x")
                    .on_press(Message::Remove(i))
                    .style(row_button(false))
            ]
            .align_y(Center),
        );
    }
    column![
        row![
            text("Queue").size(22).width(Fill),
            button("Clear")
                .on_press_maybe((!snap.queue.is_empty()).then_some(Message::ClearQueue))
                .style(pill(false)),
        ]
        .align_y(Center),
        scrollable(c).style(scroll).height(Fill).width(Fill),
    ]
    .spacing(PAD)
    .width(280)
    .into()
}

fn bar(s: &State) -> Element<'_, Message> {
    let snap = &s.snapshot;
    let cur: Option<&Track> = snap.current();
    let status = match snap.status {
        Status::Loading => "Loading...",
        Status::Buffering => "Buffering...",
        Status::Error => "Error",
        _ => "",
    };
    let info = column![
        line(cur.map_or("Nothing playing", |t| t.title.as_str())),
        line(match (status, cur) {
            ("", Some(t)) => t.artists.as_str(),
            ("", None) => "",
            (st, _) => st,
        })
        .size(13)
        .style(text::secondary),
    ];
    let info = container(info).width(220).clip(true);
    let playing = matches!(snap.status, Status::Playing | Status::Buffering);
    let max = snap.duration_secs.max(1.0);
    let pos = s.drag.unwrap_or(snap.position_secs).min(max);
    row![
        art(s, cur.and_then(|t| t.thumbnail_url.as_ref()), 48),
        info,
        button("Prev").on_press(Message::Prev).style(pill(false)),
        button(
            text(if playing { "Pause" } else { "Play" })
                .center()
                .width(Fill)
        )
        .on_press(Message::TogglePause)
        .width(84)
        .padding([9, 0])
        .style(pill(true)),
        button("Next").on_press(Message::Next).style(pill(false)),
        text(mmss(pos)).size(13).color(white(0.7)),
        slider(0.0..=max, pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .width(Fill)
            .style(rail),
        text(mmss(snap.duration_secs)).size(13).color(white(0.7)),
    ]
    .spacing(PAD)
    .align_y(Center)
    .into()
}

/// Second bar row: status line on the left, volume and toggles on the right.
fn controls(s: &State) -> Element<'_, Message> {
    let snap = &s.snapshot;
    let on = |label: String, active: bool, m: Message| {
        button(text(label).size(13))
            .on_press(m)
            .padding([4, 10])
            .style(pill(active))
    };
    let repeat = match snap.repeat {
        Repeat::Off => "Repeat: off",
        Repeat::All => "Repeat: all",
        Repeat::One => "Repeat: one",
    };
    row![
        line(
            s.notice
                .as_deref()
                .or(snap.last_error.as_deref())
                .unwrap_or("")
        )
        .size(13)
        .style(text::secondary)
        .width(Fill),
        text("Vol").size(13),
        slider(0..=100u8, snap.volume, Message::Volume)
            .width(100)
            .style(rail),
        on("Shuffle".into(), snap.shuffle, Message::Shuffle),
        on(repeat.into(), snap.repeat != Repeat::Off, Message::Repeat),
        on("Normalize".into(), snap.normalize, Message::Normalize),
        on("Lyrics".into(), s.show_lyrics, Message::ToggleLyrics),
    ]
    .spacing(PAD)
    .align_y(Center)
    .into()
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

// Look: frosted glass over a blurred backdrop. iced has no backdrop filter, so the blur
// lives in the backdrop image and the panels are translucent sheets with a light edge.

const ACCENT_A: Color = Color::from_rgb(0.49, 0.91, 1.0);
const ACCENT_B: Color = Color::from_rgb(0.65, 0.55, 0.98);
const INK: Color = Color::from_rgb(0.04, 0.06, 0.12);

fn white(a: f32) -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, a)
}

fn accent() -> Background {
    gradient::Linear::new(std::f32::consts::FRAC_PI_2)
        .add_stop(0.0, ACCENT_A)
        .add_stop(1.0, ACCENT_B)
        .into()
}

pub fn theme() -> Theme {
    Theme::custom(
        "cmusic",
        iced::theme::Palette {
            background: INK,
            text: white(0.92),
            primary: ACCENT_A,
            ..Theme::Dark.palette()
        },
    )
}

/// Shown behind the backdrop image, and alone before any art has loaded.
fn base(_: &Theme) -> container::Style {
    container::Style {
        background: Some(
            gradient::Linear::new(2.4)
                .add_stop(0.0, Color::from_rgb(0.07, 0.10, 0.24))
                .add_stop(0.55, Color::from_rgb(0.16, 0.09, 0.30))
                .add_stop(1.0, Color::from_rgb(0.03, 0.20, 0.26))
                .into(),
        ),
        ..Default::default()
    }
}

/// Dims the backdrop so text stays readable on bright covers.
fn scrim(_: &Theme) -> container::Style {
    container::Style {
        background: Some(
            gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, Color::from_rgba(0.02, 0.03, 0.08, 0.22))
                .add_stop(1.0, Color::from_rgba(0.02, 0.03, 0.08, 0.50))
                .into(),
        ),
        ..Default::default()
    }
}

fn glass(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(white(0.92)),
        // Brighter at the top edge, like light catching a pane.
        background: Some(
            gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, white(0.16))
                .add_stop(0.12, white(0.08))
                .add_stop(1.0, white(0.04))
                .into(),
        ),
        border: border::rounded(20).color(white(0.22)).width(1),
        // No drop shadow: without a GPU driver iced renders in software, where a large
        // blurred shadow per panel is the most expensive thing on screen.
        ..Default::default()
    }
}

/// Capsule button. `on` fills it with the accent (primary action, active toggle).
fn pill(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let lift = match status {
            button::Status::Hovered => 0.08,
            button::Status::Pressed => 0.14,
            _ => 0.0,
        };
        let disabled = status == button::Status::Disabled;
        button::Style {
            background: Some(if on {
                accent()
            } else {
                white(0.10 + lift).into()
            }),
            text_color: match (on, disabled) {
                (_, true) => white(0.3),
                (true, _) => INK,
                _ => white(0.92),
            },
            border: border::rounded(999)
                .color(white(if on { 0.5 + lift } else { 0.2 + lift }))
                .width(1),
            shadow: if on {
                Shadow {
                    color: Color {
                        a: 0.35 + lift,
                        ..ACCENT_A
                    },
                    offset: Vector::ZERO,
                    blur_radius: 16.0,
                }
            } else {
                Shadow::default()
            },
            ..Default::default()
        }
    }
}

/// List row: invisible until hovered. `on` marks the current queue entry.
fn row_button(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let a = match status {
            button::Status::Hovered => 0.10,
            button::Status::Pressed => 0.16,
            _ => 0.0,
        } + if on { 0.14 } else { 0.0 };
        button::Style {
            background: (a > 0.0).then(|| white(a).into()),
            text_color: white(0.92),
            border: border::rounded(12)
                .color(if on {
                    Color { a: 0.7, ..ACCENT_A }
                } else {
                    Color::TRANSPARENT
                })
                .width(1),
            ..Default::default()
        }
    }
}

fn field(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: white(if focused { 0.14 } else { 0.08 }).into(),
        border: border::rounded(999)
            .color(if focused { ACCENT_A } else { white(0.2) })
            .width(1),
        icon: white(0.6),
        placeholder: white(0.45),
        value: white(0.95),
        selection: Color { a: 0.4, ..ACCENT_A },
    }
}

fn rail(_: &Theme, status: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (accent(), white(0.16).into()),
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
            background: Color::WHITE.into(),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut st = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: border::rounded(4),
        scroller: scrollable::Scroller {
            background: white(0.22).into(),
            border: border::rounded(4),
        },
    };
    st.vertical_rail = rail;
    st.horizontal_rail = rail;
    st
}
