//! Embedded 24x24 line icons. Most paths are from Lucide (https://lucide.dev), ISC licence:
//!
//! Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part of Feather (MIT).
//! All other copyright (c) for Lucide are held by Lucide Contributors 2022.
//! Permission to use, copy, modify, and/or distribute this software for any purpose with or
//! without fee is hereby granted, provided that the above copyright notice and this permission
//! notice appear in all copies. THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
//! WARRANTIES, including all implied warranties of merchantability and fitness.
//!
//! `play_next` and `equalizer` are drawn here. Drawn in black: the view tints them with
//! `svg::Style { color }`, which replaces every opaque pixel's colour.

use iced::widget::svg::{Handle, Svg};
use std::sync::LazyLock;

/// One `fn name() -> Svg` per icon; the parsed handle is built once and cloned (cheap) per use.
macro_rules! icons {
    ($($name:ident, $fill:literal, $body:literal;)*) => {$(
        pub fn $name() -> Svg<'static> {
            static H: LazyLock<Handle> = LazyLock::new(|| {
                Handle::from_memory(concat!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill=""##,
                    $fill,
                    r##"" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"##,
                    $body,
                    "</svg>"
                ).as_bytes())
            });
            Svg::new(H.clone())
        }
    )*};
}

icons! {
    play, "#000", r#"<polygon points="6 3 20 12 6 21 6 3"/>"#;
    pause, "#000", r#"<rect x="14" y="4" width="4" height="16" rx="1"/><rect x="6" y="4" width="4" height="16" rx="1"/>"#;
    skip_prev, "#000", r#"<polygon points="19 20 9 12 19 4 19 20"/><line x1="5" x2="5" y1="19" y2="5"/>"#;
    skip_next, "#000", r#"<polygon points="5 4 15 12 5 20 5 4"/><line x1="19" x2="19" y1="5" y2="19"/>"#;
    shuffle, "none", r#"<path d="m18 14 4 4-4 4"/><path d="m18 2 4 4-4 4"/><path d="M2 18h1.973a4 4 0 0 0 3.3-1.7l5.454-8.6a4 4 0 0 1 3.3-1.7H22"/><path d="M2 6h1.972a4 4 0 0 1 3.6 2.2"/><path d="M22 18h-6.041a4 4 0 0 1-3.3-1.8l-.359-.45"/>"#;
    repeat, "none", r#"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>"#;
    repeat_one, "none", r#"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/>"#;
    heart, "none", r#"<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>"#;
    heart_filled, "#000", r#"<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>"#;
    plus, "none", r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#;
    play_next, "none", r##"<path d="M3 6h11"/><path d="M3 12h7"/><path d="M3 18h11"/><polygon points="16 9 22 13.5 16 18 16 9" fill="#000"/>"##;
    x, "none", r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#;
    search, "none", r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#;
    chevron_left, "none", r#"<path d="m15 18-6-6 6-6"/>"#;
    volume, "none", r#"<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>"#;
    music, "none", r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#;
    palette, "none", r#"<circle cx="13.5" cy="6.5" r=".5"/><circle cx="17.5" cy="10.5" r=".5"/><circle cx="8.5" cy="7.5" r=".5"/><circle cx="6.5" cy="12.5" r=".5"/><path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.926 0 1.648-.746 1.648-1.688 0-.437-.18-.835-.437-1.125-.29-.289-.438-.652-.438-1.125a1.64 1.64 0 0 1 1.668-1.668h1.996c3.051 0 5.555-2.503 5.555-5.554C21.965 6.012 17.461 2 12 2z"/>"#;
    user, "none", r#"<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>"#;
    list, "none", r#"<path d="M8 6h13"/><path d="M8 12h13"/><path d="M8 18h13"/><path d="M3 6h.01"/><path d="M3 12h.01"/><path d="M3 18h.01"/>"#;
    equalizer, "none", r#"<path d="M5 20V11"/><path d="M12 20V4"/><path d="M19 20V14"/>"#;
}
