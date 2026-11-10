use crate::app::{thumb, Message, State, LIST, LYRICS, LYRIC_LH, NARROW, SEARCH};
use crate::icons;
use crate::theme::Look;
use cmusic_core::{lyrics, Item, PageRequest, Repeat, Status, Track};
use iced::widget::svg::{self, Svg};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, hover, image, opaque, row, scrollable, slider, space, stack, text,
    text_input, tooltip,
};
use iced::{
    border, gradient, Background, Center, Color, ContentFit, Element, Fill, Padding, Right, Theme,
};
use std::time::Duration;

/// Rows rendered per list.
// ponytail: no virtualization; a lazy/windowed list is the upgrade if 500 rows ever feels slow.
const MAX_ROWS: usize = 500;

/// Type scale: section title, body, secondary (times for the lyrics view aside).
const T: f32 = 22.0;
const B: f32 = 15.0;
const S: f32 = 13.0;
/// Height shared by the main and queue headers so they sit on one baseline.
const HEAD: f32 = 36.0;
/// Width of the duration column and of the three row action buttons.
const DUR: f32 = 44.0;
const ACT: f32 = 28.0;
const SIDEBAR_W: f32 = 180.0;
const QUEUE_W: f32 = 280.0;

pub fn view(s: &State) -> Element<'_, Message> {
    let l = s.look;
    // Narrow windows have no room for a third column: the queue swaps in for the main area.
    let middle = if narrow(s) && s.queue_open && s.login.is_none() {
        row![panel(l, queue(s, main_w(s))).width(Fill)]
    } else if narrow(s) {
        row![panel(l, main_area(s)).width(Fill)]
    } else {
        row![
            panel(l, main_area(s)).width(Fill),
            panel(l, queue(s, QUEUE_W))
        ]
        .spacing(l.gap)
    };
    let content = column![
        row![panel(l, sidebar(s)), middle.width(Fill)]
            .spacing(l.gap)
            .height(Fill),
        panel(l, bar(s)),
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

fn narrow(s: &State) -> bool {
    s.size.width < NARROW
}

/// Inner width of the main panel (also what the queue gets when it replaces it): the window
/// minus outer padding, panel paddings, gaps and the sidebar (and the queue column when shown).
fn main_w(s: &State) -> f32 {
    let g = s.look.gap;
    if narrow(s) {
        s.size.width - 7.0 * g - SIDEBAR_W
    } else {
        s.size.width - 10.0 * g - SIDEBAR_W - QUEUE_W
    }
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
    text(t).size(sz(l, B))
}

fn line<'a>(l: Look, t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    txt(l, t).wrapping(Wrapping::None)
}

/// Secondary text: dimmed, small.
fn dim<'a>(l: Look, t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(t).size(sz(l, S)).color(l.text_dim)
}

/// Rail width plus the gap an embedded scrollbar takes from its content.
const RAIL: f32 = 16.0;

/// Shortens `t` to fit `width` px at base text size `base` (iced has no ellipsis), cutting on a
/// char boundary and ending with "...".
// ponytail: widths are per-class estimates of a DejaVu Sans (the usual Linux default), not measured; the text columns
// are clipped so a misjudged glyph can never overlap its neighbour. Real measurement (the
// advanced text API) is the upgrade.
fn fit(l: Look, t: &str, width: f32, base: f32) -> String {
    let em = sz(l, base);
    let w = |c: char| {
        em * match c {
            'i' | 'j' | 'l' | 'I' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' => 0.3,
            ' ' => 0.33,
            'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '-' | '/' => 0.4,
            'm' | 'w' | 'M' | 'W' | '@' => 0.9,
            'A'..='Z' | '&' => 0.7,
            '0'..='9' => 0.64,
            c if c as u32 >= 0x2e80 => 1.0,
            _ => 0.62,
        }
    };
    if t.chars().map(w).sum::<f32>() <= width {
        return t.to_string();
    }
    let room = width - 3.0 * em * 0.3;
    let head: String = t
        .chars()
        .scan(0.0, |used, c| {
            *used += w(c);
            (*used <= room).then_some(c)
        })
        .collect();
    format!("{}...", head.trim_end())
}

// ponytail: border_radius is ignored by the software renderer (it applies on GPU only).
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

/// `art`, with an equalizer glyph over it when this row is the track being played.
fn art_mark<'a>(
    s: &'a State,
    url: Option<&String>,
    size: u32,
    playing: bool,
) -> Element<'a, Message> {
    let a = art(s, url, size);
    if !playing {
        return a;
    }
    stack![
        a,
        // ponytail: the one fixed-colour spot (white glyph on a black square), by design.
        container(ico(icons::equalizer(), size as f32 * 0.5, Color::WHITE))
            .center(size)
            .style(move |_| container::Style {
                // Dark whatever the theme or the cover, so the glyph always reads.
                background: Some(tint(Color::BLACK, 0.6).into()),
                border: border::rounded(size as f32 / 5.0),
                ..Default::default()
            })
    ]
    .into()
}

/// An icon tinted with `color` (the svg is black; this replaces its colour).
fn ico(icon: Svg<'static>, size: f32, color: Color) -> Svg<'static> {
    icon.width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
}

/// `content` with a themed tooltip.
fn tipped<'a>(
    l: Look,
    content: impl Into<Element<'a, Message>>,
    tip: impl Into<String>,
) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(tip.into()).size(sz(l, S)))
            .padding([3, 8])
            .style(move |_| container::Style {
                background: Some(tint(l.ink, 0.92).into()),
                text_color: Some(l.text),
                border: border::rounded(6).color(l.panel_border).width(1),
                ..Default::default()
            }),
        tooltip::Position::Top,
    )
    .gap(4)
    .delay(Duration::from_millis(400))
    .into()
}

/// Square icon-only button with a tooltip naming the action; `m` None draws it disabled.
fn ibtn<'a>(
    l: Look,
    icon: Svg<'static>,
    size: f32,
    tip: impl Into<String>,
    m: Option<Message>,
    style: impl Fn(&Theme, button::Status) -> button::Style + 'a,
    color: Color,
) -> Element<'a, Message> {
    let inner: Element<'a, Message> = container(ico(icon, size * 0.55, color)).center(Fill).into();
    let b = button(inner)
        .on_press_maybe(m)
        .width(size)
        .height(size)
        .padding(0)
        .style(style);
    tipped(l, b, tip)
}

fn sidebar(s: &State) -> Element<'_, Message> {
    let l = s.look;
    // The section being loaded counts as open, so the click answers at once.
    let open = s
        .awaiting
        .as_ref()
        .or(s.failed.as_ref().map(|(r, _)| r))
        .or(s.current.as_ref().map(|(r, _)| r));
    let nav = |label: &'static str, r: PageRequest| {
        let on = open == Some(&r);
        button(txt(l, label))
            .on_press(Message::Open(r))
            .width(Fill)
            .padding([8, 12])
            .style(row_button(l, on))
    };
    let search = stack![
        text_input("Search", &s.search)
            .id(SEARCH)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .padding(Padding {
                left: 36.0,
                ..Padding::from([8.0, 14.0])
            })
            .size(sz(l, B))
            .style(field(l)),
        container(ico(icons::search(), 16.0, tint(l.text, 0.6)))
            .padding([0, 12])
            .height(Fill)
            .align_y(Center),
    ];
    column![
        search,
        nav("Liked songs", PageRequest::LikedSongs),
        nav("Playlists", PageRequest::LibraryPlaylists),
        nav("Songs", PageRequest::LibrarySongs),
        nav("Albums", PageRequest::LibraryAlbums),
        nav("Artists", PageRequest::LibraryArtists),
        space().height(Fill),
        button(
            row![
                ico(icons::user(), 16.0, l.text),
                txt(l, if s.logged_in { "Account" } else { "Log in" })
            ]
            .spacing(l.pad)
            .align_y(Center)
        )
        .on_press(Message::OpenLogin)
        .width(Fill)
        .padding([6, 14])
        .style(pill(l, false)),
    ]
    .spacing(4)
    .width(SIDEBAR_W)
    .into()
}

/// Section title row: the title, then `trail` (a button, or nothing).
fn head<'a>(l: Look, title: String, trail: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut r = row![text(title)
        .size(sz(l, T))
        .wrapping(Wrapping::None)
        .width(Fill)]
    .spacing(l.pad)
    .align_y(Center)
    .height(HEAD);
    if let Some(t) = trail {
        r = r.push(t);
    }
    r.into()
}

/// Centred message for loading, empty and first-start states.
fn hint<'a>(
    l: Look,
    title: Option<String>,
    msg: impl text::IntoFragment<'a>,
) -> Element<'a, Message> {
    let mut c = column![].spacing(6).align_x(Center);
    if let Some(t) = title {
        c = c.push(text(t).size(sz(l, T)).center());
    }
    container(c.push(dim(l, msg).center())).center(Fill).into()
}

fn request_name(r: &PageRequest) -> String {
    match r {
        PageRequest::Search(q) => format!("Search: {q}"),
        PageRequest::LibraryPlaylists => "Playlists".into(),
        PageRequest::LibrarySongs => "Songs".into(),
        PageRequest::LibraryAlbums => "Albums".into(),
        PageRequest::LibraryArtists => "Artists".into(),
        PageRequest::LikedSongs => "Liked songs".into(),
        PageRequest::Playlist(_) => "Playlist".into(),
        PageRequest::Album(_) => "Album".into(),
        PageRequest::Artist(_) => "Artist".into(),
    }
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
        (None, Some((_, p))) if s.failed.is_none() => p.title.clone(),
        _ => String::new(),
    };
    let back = (!s.history.is_empty()).then(|| {
        ibtn(
            l,
            icons::chevron_left(),
            HEAD - 4.0,
            "Back",
            Some(Message::Back),
            row_button(l, false),
            l.text,
        )
    });
    // Panel inner width minus the scrollbar, row paddings (list and button), art, the gaps
    // around the text and the duration column.
    let avail = (main_w(s) - RAIL - 6.0 * l.pad - 40.0 - DUR).max(80.0);
    let body: Element<_> = match (&s.awaiting, &s.current) {
        (Some(r), _) => hint(l, Some(request_name(r)), "Loading..."),
        (None, _) if s.failed.is_some() => {
            let (r, e) = s.failed.as_ref().expect("checked by the guard");
            hint(l, Some(request_name(r)), e.as_str())
        }
        (None, Some((_, p))) if !p.items.is_empty() => {
            let mut rows = column![].spacing(4);
            for (i, it) in p.items.iter().enumerate().take(MAX_ROWS) {
                rows = rows.push(item_row(s, i, it, avail));
            }
            let list = scrollable(rows.padding([0.0, l.pad]))
                .id(LIST)
                .spacing(6)
                .style(scroll(l))
                .height(Fill)
                .width(Fill);
            if p.items.len() > MAX_ROWS {
                column![
                    list,
                    dim(l, format!("showing {MAX_ROWS} of {}", p.items.len()))
                ]
                .spacing(l.pad)
                .into()
            } else {
                list.into()
            }
        }
        (None, Some(_)) => hint(l, None, "Nothing here"),
        (None, None) => hint(
            l,
            Some("Welcome to cmusic".into()),
            "Type in the search box on the left, or pick something from your library.",
        ),
    };
    column![
        row![]
            .extend(back)
            .push(head(l, title, None))
            .spacing(l.pad)
            .align_y(Center),
        body
    ]
    .spacing(l.pad)
    .width(Fill)
    .into()
}

/// A list row: one button over the whole row (hover highlights all of it) and, for tracks,
/// action buttons that appear only while the pointer is over the row.
fn list_row<'a>(
    l: Look,
    body: impl Into<Element<'a, Message>>,
    press: Message,
    actions: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let b = button(body)
        .on_press(press)
        .width(Fill)
        .padding([4.0, l.pad])
        .clip(true)
        .style(row_button(l, false));
    match actions {
        None => b.into(),
        // The actions sit in an opaque-ish pill at the right end, over the duration and the end
        // of the title, so nothing shows through. `opaque` makes the padding and border swallow clicks too.
        Some(a) => hover(
            b,
            container(opaque(container(a).padding(2).style(move |_| {
                container::Style {
                    background: Some(tint(l.ink, 0.88).into()),
                    border: border::rounded(l.radius_row)
                        .color(l.control_border)
                        .width(1),
                    ..Default::default()
                }
            })))
            .width(Fill)
            .height(Fill)
            .align_x(Right)
            .align_y(Center)
            .padding(Padding {
                right: l.pad,
                ..Padding::ZERO
            }),
        ),
    }
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
    let playing = matches!(it, Item::Track(t) if Some(&t.video_id) == s.snapshot.current().map(|c| &c.video_id));
    // Non-track rows have no duration column.
    // They get a chevron instead (16 px plus its spacing).
    let w = if matches!(it, Item::Track(_)) {
        w
    } else {
        w + DUR - 16.0 - l.pad
    };
    let mut name = line(l, fit(l, title, w, B));
    if playing {
        name = name.color(l.accent_a);
    }
    let mut body = row![
        art_mark(s, thumb(it).as_ref(), 40, playing),
        container(column![
            name,
            dim(l, fit(l, sub, w, S)).wrapping(Wrapping::None)
        ])
        .width(Fill)
        .clip(true)
    ]
    .spacing(l.pad)
    .align_y(Center);
    let press = open.map_or(Message::PlayRow(i), Message::Open);
    let Item::Track(t) = it else {
        let chevron: Element<'a, Message> = ico(icons::chevron_right(), 16.0, l.text_dim).into();
        body = body.push(chevron);
        return list_row(l, body, press, None);
    };
    body = body.push(
        dim(
            l,
            t.duration_secs.map(|d| mmss(d as f64)).unwrap_or_default(),
        )
        .width(DUR)
        .align_x(Right),
    );
    let ghost = || row_button(l, false);
    let liked = s.liked.contains(&t.video_id);
    let actions = row![
        ibtn(
            l,
            icons::play_next(),
            ACT,
            "Play next",
            Some(Message::PlayNext(t.clone())),
            ghost(),
            l.text
        ),
        ibtn(
            l,
            icons::plus(),
            ACT,
            "Add to queue",
            Some(Message::Enqueue(t.clone())),
            ghost(),
            l.text
        ),
        ibtn(
            l,
            if liked {
                icons::heart_filled()
            } else {
                icons::heart()
            },
            ACT,
            if liked { "Unlike" } else { "Like" },
            Some(Message::Like(t.video_id.clone())),
            ghost(),
            if liked { l.accent_a } else { l.text },
        ),
    ]
    .spacing(2);
    list_row(l, body, press, Some(actions.into()))
}

fn lyrics_view(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let body: Element<_> = match s.current_lyrics() {
        Some(ly) if !ly.lines.is_empty() => {
            let cur = lyrics::current_line(ly, s.snapshot.position_secs);
            // Plain lyrics have no times: all lines at normal strength, none highlighted.
            let synced = ly.lines.iter().any(|(t, _)| t.is_some());
            let w = main_w(s) - RAIL - 2.0 * l.pad;
            // Fixed line height (no wrapping) so the auto-scroll offset is exact.
            let mut c = column![];
            for (i, (_, t)) in ly.lines.iter().enumerate() {
                let t = text(fit(l, t, w, 21.0))
                    .size(sz(l, 18.0))
                    .wrapping(Wrapping::None);
                let t = match (synced, Some(i) == cur) {
                    (true, true) => t.size(sz(l, 21.0)).color(l.accent_a),
                    (true, false) => t.color(tint(l.text_dim, 0.55)),
                    _ => t.color(l.text),
                };
                c = c.push(
                    container(t)
                        .width(Fill)
                        .height(LYRIC_LH * l.font_scale)
                        .center_x(Fill)
                        .align_y(Center),
                );
            }
            scrollable(c.padding(l.pad))
                .id(LYRICS)
                .on_scroll(|v| Message::LyricsScrolled(v.bounds().height))
                .spacing(6)
                .style(scroll(l))
                .height(Fill)
                .width(Fill)
                .into()
        }
        _ => hint(l, None, "No lyrics"),
    };
    column![head(l, "Lyrics".into(), None), body]
        .spacing(l.pad)
        .width(Fill)
        .into()
}

fn login(s: &State) -> Element<'_, Message> {
    let l = s.look;
    let lg = s.login.as_ref().expect("checked by caller");
    let form = column![
        text("Log in").size(sz(l, T)),
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
            .size(sz(l, B))
            .style(field(l)),
        dim(l, "DevTools > Network > a request > Cookie header"),
        row![
            button(txt(l, "Use pasted cookie"))
                .on_press(Message::UseCookie)
                .style(pill(l, false)),
            button(txt(l, "Close"))
                .on_press(Message::CloseLogin)
                .style(pill(l, false)),
        ]
        .spacing(l.pad),
    ]
    .spacing(l.pad + 4.0)
    .width(Fill)
    .max_width(480);
    container(form).center(Fill).padding(l.pad).into()
}

fn queue(s: &State, qw: f32) -> Element<'_, Message> {
    let l = s.look;
    let snap = &s.snapshot;
    let mut c = column![].spacing(2);
    // Queue column minus art, row paddings and the rail.
    let w = qw - 36.0 - 3.0 * l.pad - RAIL;
    for (i, t) in snap.queue.iter().enumerate().take(MAX_ROWS) {
        let cur = snap.cursor == Some(i);
        let mut name = line(l, fit(l, &t.title, w, B));
        if cur {
            name = name.color(l.accent_a);
        }
        let body = row![
            art_mark(s, t.thumbnail_url.as_ref(), 36, cur),
            container(column![
                name,
                dim(l, fit(l, &t.artists, w, S)).wrapping(Wrapping::None)
            ])
            .width(Fill)
            .clip(true)
        ]
        .spacing(l.pad)
        .align_y(Center);
        let x = ibtn(
            l,
            icons::x(),
            ACT,
            "Remove from queue",
            Some(Message::Remove(i)),
            row_button(l, false),
            l.text,
        );
        c = c.push(
            container(list_row(l, body, Message::Jump(i), Some(x)))
                .width(Fill)
                .style(move |_| container::Style {
                    background: cur.then(|| selected(l).into()),
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
    let body: Element<_> = if snap.queue.is_empty() {
        hint(l, None, "Queue is empty - play something")
    } else {
        scrollable(c)
            .spacing(6)
            .style(scroll(l))
            .height(Fill)
            .width(Fill)
            .into()
    };
    column![
        head(
            l,
            "Queue".into(),
            Some(
                button(txt(l, "Clear"))
                    .on_press_maybe((!snap.queue.is_empty()).then_some(Message::ClearQueue))
                    .style(pill(l, false))
                    .into()
            )
        ),
        body,
    ]
    .spacing(l.pad)
    .width(qw)
    .into()
}

/// Now-playing bar: art and text on the left, transport over the seek bar in the middle,
/// volume and toggles on the right. Narrow windows put the seek bar (and the volume) on a second row.
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
    let w = 220.0;
    // Fixed three lines so a notice never moves anything.
    let info = column![
        line(
            l,
            fit(l, cur.map_or("Nothing playing", |t| t.title.as_str()), w, B)
        ),
        dim(
            l,
            fit(
                l,
                match (status, cur) {
                    ("", Some(t)) => t.artists.as_str(),
                    ("", None) => "",
                    (st, _) => st,
                },
                w,
                S
            )
        )
        .wrapping(Wrapping::None)
        .height(sz(l, S) * 1.4),
        dim(
            l,
            fit(
                l,
                s.notice
                    .as_deref()
                    .or(snap.last_error.as_deref())
                    .unwrap_or(""),
                w,
                S
            )
        )
        .wrapping(Wrapping::None)
        .height(sz(l, S) * 1.4),
    ];
    let info = container(info).width(w).clip(true);
    let playing = matches!(snap.status, Status::Playing | Status::Buffering);
    let max = snap.duration_secs.max(1.0);
    let pos = s.drag.unwrap_or(snap.position_secs).min(max);
    let transport = row![
        ibtn(
            l,
            icons::skip_prev(),
            36.0,
            "Previous",
            Some(Message::Prev),
            pill(l, false),
            l.text
        ),
        ibtn(
            l,
            if playing {
                icons::pause()
            } else {
                icons::play()
            },
            46.0,
            if playing { "Pause" } else { "Play" },
            Some(Message::TogglePause),
            pill(l, true),
            l.ink
        ),
        ibtn(
            l,
            icons::skip_next(),
            36.0,
            "Next",
            Some(Message::Next),
            pill(l, false),
            l.text
        ),
    ]
    .spacing(l.pad + 4.0)
    .align_y(Center);
    let seek = row![
        dim(l, mmss(pos)).width(44).align_x(Right),
        slider(0.0..=max, pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .width(Fill)
            .style(rail(l)),
        dim(l, mmss(snap.duration_secs)).width(44),
    ]
    .spacing(l.pad + 4.0)
    .align_y(Center);
    let tog = |icon: Svg<'static>, tip: &str, on: bool, m: Message| {
        ibtn(
            l,
            icon,
            32.0,
            tip,
            Some(m),
            pill(l, on),
            if on { l.ink } else { l.text },
        )
    };
    let repeat = match snap.repeat {
        Repeat::Off => ("Repeat: off", icons::repeat()),
        Repeat::All => ("Repeat: all", icons::repeat()),
        Repeat::One => ("Repeat: one", icons::repeat_one()),
    };
    let mut toggles = row![
        tog(icons::shuffle(), "Shuffle", snap.shuffle, Message::Shuffle),
        tog(
            repeat.1,
            repeat.0,
            snap.repeat != Repeat::Off,
            Message::Repeat
        ),
        tipped(
            l,
            button(text("Normalize").size(sz(l, S)))
                .on_press(Message::Normalize)
                .padding([6, 10])
                .style(pill(l, snap.normalize)),
            "Even out loudness between tracks"
        ),
        tog(
            icons::music(),
            "Lyrics",
            s.show_lyrics,
            Message::ToggleLyrics
        ),
        tog(
            icons::palette(),
            &format!("Theme: {}", s.theme_name),
            false,
            Message::CycleTheme
        ),
    ]
    .spacing(6)
    .align_y(Center);
    if narrow(s) {
        toggles = toggles.push(tog(
            icons::list(),
            "Queue",
            s.queue_open && s.login.is_none(),
            Message::ToggleQueue,
        ));
    }
    let volume = row![
        ico(icons::volume(), 16.0, l.text_dim),
        slider(0..=100u8, snap.volume, Message::Volume)
            .width(110)
            .style(rail(l)),
    ]
    .spacing(l.pad)
    .align_y(Center);
    // Art and title open the lyrics.
    let now = button(
        row![art(s, cur.and_then(|t| t.thumbnail_url.as_ref()), 64), info]
            .spacing(l.pad + 4.0)
            .align_y(Center),
    )
    .on_press(Message::ToggleLyrics)
    .padding(0)
    .style(row_button(l, false));
    let gap = l.pad + 4.0;
    if narrow(s) {
        // No room for a slider between the columns: the seek bar gets its own full-width row.
        return column![
            row![now, container(transport).center_x(Fill), toggles]
                .spacing(gap)
                .align_y(Center),
            row![seek.width(Fill), volume].spacing(gap).align_y(Center),
        ]
        .spacing(l.pad)
        .into();
    }
    row![
        now,
        column![transport, seek]
            .spacing(4)
            .align_x(Center)
            .width(Fill),
        column![toggles, volume]
            .spacing(l.pad)
            .align_x(Right)
            .width(250),
    ]
    .spacing(gap)
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
                    tint(l.text, 0.5 + lift)
                } else {
                    tint(l.control_border, l.control_border.a + lift)
                })
                .width(1),
            ..Default::default()
        }
    }
}

/// Fill of the selected row (current queue entry, open section): a light accent tint, so the
/// accent title on it stays readable in dark and light looks alike.
fn selected(l: Look) -> Color {
    tint(l.accent_a, 0.16)
}

/// List row: invisible until hovered. `on` marks the open sidebar section.
fn row_button(l: Look, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let a = match status {
            button::Status::Hovered => 0.10,
            button::Status::Pressed => 0.16,
            _ => 0.0,
        };
        button::Style {
            background: if on {
                Some(selected(l).into())
            } else {
                (a > 0.0).then(|| tint(l.control, a).into())
            },
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
            placeholder: tint(l.text, 0.65),
            value: tint(l.text, 0.95),
            selection: tint(l.accent_a, 0.4),
        }
    }
}

fn rail(l: Look) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, status| slider::Style {
        rail: slider::Rail {
            backgrounds: (accent(l), tint(l.control, 0.24).into()),
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
            background: l.text.into(),
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
    fn fit_cuts_on_char_boundaries_and_keeps_what_fits() {
        let l = Look::default();
        assert_eq!(fit(l, "short", 200.0, 16.0), "short");
        let long = "Waiting For the End (Official Music Video) [4K Upgrade]";
        let out = fit(l, long, 300.0, 16.0);
        assert!(out.ends_with("...") && out.len() < long.len());
        assert!(fit(l, long, 3000.0, 16.0) == long);
        // Multibyte text is cut on a char boundary and never panics.
        let out = fit(l, "ñandú canción mañana", 80.0, 16.0);
        assert!(out.ends_with("..."));
        let cjk = fit(l, "日本語日本語日本語日本語日本語", 80.0, 16.0);
        assert!(cjk.starts_with("日本語") && cjk.ends_with("..."));
        // Narrow glyphs buy more characters than wide ones in the same width.
        assert!(
            fit(l, &"i".repeat(60), 200.0, 16.0).len() > fit(l, &"W".repeat(60), 200.0, 16.0).len()
        );
    }
}
