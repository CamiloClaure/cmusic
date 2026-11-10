use crate::app::{thumb, Message, State};
use cmusic_core::{lyrics, Item, PageRequest, Repeat, Status, Track};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, image, row, scrollable, slider, space, text, text_input,
};
use iced::{Center, Element, Fill};

/// Rows rendered per list.
// ponytail: no virtualization; a lazy/windowed list is the upgrade if 500 rows ever feels slow.
const MAX_ROWS: usize = 500;
const PAD: f32 = 8.0;

pub fn view(s: &State) -> Element<'_, Message> {
    column![
        row![sidebar(s), main_area(s), queue(s)]
            .spacing(PAD)
            .height(Fill),
        bar(s),
        controls(s),
    ]
    .spacing(PAD)
    .padding(PAD)
    .into()
}

fn line<'a>(t: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(t).wrapping(Wrapping::None)
}

fn art<'a>(s: &'a State, url: Option<&String>, size: u32) -> Element<'a, Message> {
    match url.and_then(|u| s.art.get(u)) {
        Some(h) => image(h.clone()).width(size).height(size).into(),
        None => container(space())
            .width(size)
            .height(size)
            .style(container::rounded_box)
            .into(),
    }
}

fn sidebar(s: &State) -> Element<'_, Message> {
    let nav = |label: &'static str, r: PageRequest| {
        button(label)
            .on_press(Message::Open(r))
            .width(Fill)
            .style(button::text)
    };
    column![
        text_input("Search", &s.search)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit),
        nav("Liked songs", PageRequest::LikedSongs),
        nav("Playlists", PageRequest::LibraryPlaylists),
        nav("Songs", PageRequest::LibrarySongs),
        nav("Albums", PageRequest::LibraryAlbums),
        nav("Artists", PageRequest::LibraryArtists),
        space().height(Fill),
        button("Log in").on_press(Message::OpenLogin).width(Fill),
    ]
    .spacing(PAD)
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
        button("Back").on_press_maybe((!s.history.is_empty()).then_some(Message::Back)),
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
    .style(button::text);
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
        .style(button::secondary)
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
                    t.style(text::primary)
                } else {
                    t.style(text::secondary)
                });
            }
            // ponytail: no auto-scroll to the current line; add scrollable::scroll_to if wanted.
            scrollable(c.padding(PAD)).height(Fill).width(Fill).into()
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
        button("Import from browser").on_press(Message::ImportBrowsers),
        text("Or paste a Cookie header:"),
        text_input("Cookie: ...", &l.cookie)
            .secure(true)
            .on_input(Message::CookieInput)
            .on_submit(Message::UseCookie),
        button("Use pasted cookie").on_press(Message::UseCookie),
        button("Close")
            .on_press(Message::CloseLogin)
            .style(button::secondary),
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
            .style(if cur { button::primary } else { button::text });
        c = c.push(
            row![
                name,
                button("x").on_press(Message::Remove(i)).style(button::text)
            ]
            .align_y(Center),
        );
    }
    column![
        row![
            text("Queue").size(22).width(Fill),
            button("Clear").on_press_maybe((!snap.queue.is_empty()).then_some(Message::ClearQueue)),
        ]
        .align_y(Center),
        scrollable(c).height(Fill).width(Fill),
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
    ]
    .width(220);
    let playing = matches!(snap.status, Status::Playing | Status::Buffering);
    let max = snap.duration_secs.max(1.0);
    let pos = s.drag.unwrap_or(snap.position_secs).min(max);
    row![
        art(s, cur.and_then(|t| t.thumbnail_url.as_ref()), 48),
        info,
        button("Prev").on_press(Message::Prev),
        button(if playing { "Pause" } else { "Play" }).on_press(Message::TogglePause),
        button("Next").on_press(Message::Next),
        text(mmss(pos)),
        slider(0.0..=max, pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .width(Fill),
        text(mmss(snap.duration_secs)),
    ]
    .spacing(PAD)
    .align_y(Center)
    .into()
}

/// Second bar row: status line on the left, volume and toggles on the right.
fn controls(s: &State) -> Element<'_, Message> {
    let snap = &s.snapshot;
    let on = |label: String, active: bool, m: Message| {
        button(text(label).size(13)).on_press(m).style(if active {
            button::primary
        } else {
            button::secondary
        })
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
        slider(0..=100u8, snap.volume, Message::Volume).width(100),
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
