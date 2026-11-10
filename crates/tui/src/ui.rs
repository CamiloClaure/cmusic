use crate::app::{App, Focus, SIDEBAR};
use cmusic_core::{lyrics, Item, Repeat, Status};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, LineGauge, List, ListState, Paragraph};
use ratatui::Frame;

const HELP: &str = "\
Tab / Shift-Tab, h/l, Left/Right  move focus
j/k, Up/Down, g/G, PageUp/PageDown  move in a list
/  search (Enter runs it, Esc cancels)
Enter  open / play / jump      Backspace  back
a  enqueue   n  play next   +/-  like/dislike
d  remove from queue   C  clear queue
Space  pause    >/<  next/prev
./,  seek +5/-5 s   Shift-Right/Left  +30/-30 s
=/_  volume +5/-5   s  shuffle   r  repeat   N  normalize
?  this help   q or Ctrl-C  quit";

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    Block::bordered().title(title).border_style(style)
}

fn list<'a>(rows: Vec<Line<'a>>, block: Block<'a>, marked: Option<usize>) -> List<'a> {
    let rows = rows.into_iter().enumerate().map(|(i, l)| {
        if Some(i) == marked {
            l.style(Style::new().add_modifier(Modifier::BOLD).fg(Color::Green))
        } else {
            l
        }
    });
    List::new(rows)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
}

pub fn draw(app: &App, f: &mut Frame) {
    let [body, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(f.area());
    let w = body.width;
    let mut cols = vec![];
    if w >= 60 {
        cols.push(Constraint::Length(16));
    }
    cols.push(Constraint::Min(0));
    if w >= 100 {
        cols.push(Constraint::Length(34));
    }
    let areas = Layout::horizontal(cols).split(body);
    let mut it = areas.iter().copied();
    if w >= 60 {
        draw_sidebar(app, f, it.next().unwrap_or_default());
    }
    draw_main(app, f, it.next().unwrap_or_default());
    if w >= 100 {
        draw_queue(app, f, it.next().unwrap_or_default());
    }
    draw_status(app, f, status);
    if app.help {
        let area = f.area().centered(
            Constraint::Max(72),
            Constraint::Length(HELP.lines().count() as u16 + 2),
        );
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(HELP).block(Block::bordered().title("Help")),
            area,
        );
    }
}

fn draw_sidebar(app: &App, f: &mut Frame, area: Rect) {
    let rows = SIDEBAR.iter().map(|s| Line::from(*s)).collect();
    let mut st = ListState::default().with_selected(Some(app.sidebar_sel));
    let l = list(rows, block("cmusic", app.focus == Focus::Sidebar), None);
    f.render_stateful_widget(l, area, &mut st);
}

fn draw_queue(app: &App, f: &mut Frame, area: Rect) {
    let rows = app
        .snap
        .queue
        .iter()
        .map(|t| Line::from(format!("{} - {}", t.title, t.artists)))
        .collect();
    let focused = app.focus == Focus::Queue;
    let mut st = ListState::default().with_selected(focused.then_some(app.queue_sel));
    let l = list(rows, block("Queue", focused), app.snap.cursor);
    f.render_stateful_widget(l, area, &mut st);
}

fn draw_main(app: &App, f: &mut Frame, area: Rect) {
    let lyrics_h = if area.height >= 16 { 8 } else { 0 };
    let [top, bottom] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(lyrics_h)]).areas(area);
    let focused = app.focus == Focus::Main;
    let title = match (&app.input, &app.page) {
        (Some(q), _) => format!("Search: {q}_"),
        (None, Some(p)) if !p.title.is_empty() => p.title.clone(),
        _ => "Main".into(),
    };
    let b = block(&title, focused || app.input.is_some());
    if app.awaiting.is_some() {
        f.render_widget(Paragraph::new("Loading...").block(b), top);
    } else if let Some(page) = &app.page {
        let rows = page.items.iter().map(item_line).collect();
        let mut st = ListState::default().with_selected(focused.then_some(app.main_sel));
        f.render_stateful_widget(list(rows, b, None), top, &mut st);
    } else {
        f.render_widget(
            Paragraph::new("Press / to search, ? for help").block(b),
            top,
        );
    }
    if lyrics_h > 0 {
        draw_lyrics(app, f, bottom);
    }
}

fn item_line(item: &Item) -> Line<'_> {
    match item {
        Item::Track(t) => {
            let d = t.duration_secs.map_or(String::new(), |d| mmss(d as f64));
            Line::from(format!("{} - {}  {d}", t.title, t.artists))
        }
        Item::Playlist(e) => Line::from(format!("[Playlist] {} {}", e.title, e.subtitle)),
        Item::Album(e) => Line::from(format!("[Album] {} {}", e.title, e.subtitle)),
        Item::Artist(e) => Line::from(format!("[Artist] {} {}", e.title, e.subtitle)),
    }
}

fn draw_lyrics(app: &App, f: &mut Frame, area: Rect) {
    let b = Block::bordered().title("Lyrics");
    let h = b.inner(area).height as usize;
    let current = app.snap.current().map(|t| t.video_id.as_str());
    let found = app
        .lyrics
        .as_ref()
        .filter(|(id, _)| Some(id.as_str()) == current);
    let Some((_, ly)) = found else {
        f.render_widget(Paragraph::new("No lyrics").block(b), area);
        return;
    };
    let cur = lyrics::current_line(ly, app.snap.position_secs);
    let first = cur.map_or(0, |c| c.saturating_sub(h / 2));
    let lines: Vec<Line> = ly
        .lines
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, (_, text))| {
            let l = Line::from(text.as_str());
            if Some(i) == cur {
                l.style(Style::new().add_modifier(Modifier::BOLD).fg(Color::Yellow))
            } else {
                l
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(b), area);
}

fn draw_status(app: &App, f: &mut Frame, area: Rect) {
    let b = Block::new().borders(Borders::TOP);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let [row1, row2] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let s = &app.snap;
    let (mark, name) = match s.status {
        Status::Idle => ("-", "Idle"),
        Status::Loading => ("~", "Loading..."),
        Status::Playing => (">", "Playing"),
        Status::Paused => ("||", "Paused"),
        Status::Buffering => ("~", "Buffering..."),
        Status::Error => ("!", "Error"),
    };
    let song = s.current().map_or("nothing playing".to_string(), |t| {
        format!("{} - {}", t.title, t.artists)
    });
    let [title, bar, time] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(24),
        Constraint::Length(14),
    ])
    .areas(row1);
    f.render_widget(Line::from(format!("{mark} {song}")), title);
    let ratio = if s.duration_secs > 0.0 {
        (s.position_secs / s.duration_secs).clamp(0.0, 1.0)
    } else {
        0.0
    };
    f.render_widget(LineGauge::default().ratio(ratio).label(""), bar);
    f.render_widget(
        Line::from(format!(
            " {} / {}",
            mmss(s.position_secs),
            mmss(s.duration_secs)
        )),
        time,
    );

    let repeat = match s.repeat {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    };
    let onoff = |b| if b { "on" } else { "off" };
    let msg = if !app.notice.is_empty() {
        app.notice.as_str()
    } else {
        s.last_error.as_deref().unwrap_or("")
    };
    let flags = format!(
        "{name}  vol {}  shuffle {}  repeat {repeat}  norm {}",
        s.volume,
        onoff(s.shuffle),
        onoff(s.normalize)
    );
    let [left, right] = Layout::horizontal([
        Constraint::Length(flags.chars().count() as u16),
        Constraint::Min(0),
    ])
    .areas(row2);
    f.render_widget(Line::from(flags), left);
    f.render_widget(
        Line::from(Span::styled(msg, Style::new().fg(Color::Red))).alignment(Alignment::Right),
        right,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmusic_core::{Entry, Lyrics, Page, Snapshot, Track, Update};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn track(title: &str) -> Track {
        Track {
            video_id: "id".into(),
            title: title.into(),
            artists: "アーティスト".into(),
            album: None,
            duration_secs: Some(225),
            thumbnail_url: None,
        }
    }

    fn app() -> App {
        let mut app = App::default();
        let titles = [
            "Current Song",
            "夜に駆ける",
            "Party 🎉🎉🎉 very long title that must be cut",
        ];
        app.on_update(Update::Player(Snapshot {
            status: Status::Playing,
            queue: titles.iter().map(|t| track(t)).collect(),
            cursor: Some(0),
            position_secs: 83.0,
            duration_secs: 225.0,
            volume: 70,
            ..Default::default()
        }));
        app.page = Some(Page {
            title: "Results".into(),
            items: vec![
                Item::Track(track("夜に駆ける")),
                Item::Album(Entry {
                    id: "a".into(),
                    title: "😀 Album".into(),
                    subtitle: "x".into(),
                    thumbnail_url: None,
                }),
            ],
        });
        app.lyrics = Some((
            "id".into(),
            Lyrics {
                lines: vec![
                    (Some(0.0), "こんにちは".into()),
                    (Some(80.0), "line two".into()),
                ],
            },
        ));
        app
    }

    #[test]
    fn draws_at_every_size() {
        for (w, h) in [(120, 30), (80, 24), (40, 10), (1, 1), (0, 0), (100, 3)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut a = app();
            a.help = true;
            t.draw(|f| draw(&a, f)).unwrap();
            a.help = false;
            a.input = Some("日本".into());
            t.draw(|f| draw(&a, f)).unwrap();
            a.awaiting = Some(cmusic_core::PageRequest::LikedSongs);
            t.draw(|f| draw(&a, f)).unwrap();
        }
    }

    #[test]
    fn current_title_visible() {
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let a = app();
        t.draw(|f| draw(&a, f)).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Current Song"));
        assert!(text.contains("1:23 / 3:45"));
    }
}
