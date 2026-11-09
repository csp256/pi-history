//! Interpret styling only, never emit transcript control sequences to the terminal.
use super::{LineStyle, markdown::StyledLine};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

pub(super) fn has_ansi(text: &str) -> bool {
    text.contains('\x1b') || text.chars().any(|c| matches!(c, '\u{90}'..='\u{9f}'))
}

fn sgr(style: &mut Style, parameters: &str) {
    let values: Vec<Option<u16>> = parameters
        .split(';')
        .map(|p| {
            if p.is_empty() {
                Some(0)
            } else {
                p.parse().ok()
            }
        })
        .collect();
    let mut i = 0;
    while i < values.len() {
        let Some(code) = values[i] else {
            i += 1;
            continue;
        };
        match code {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            5 => *style = style.add_modifier(Modifier::SLOW_BLINK),
            6 => *style = style.add_modifier(Modifier::RAPID_BLINK),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            8 => *style = style.add_modifier(Modifier::HIDDEN),
            9 => *style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            25 => *style = style.remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            28 => *style = style.remove_modifier(Modifier::HIDDEN),
            29 => *style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(Color::Indexed((code - 30) as u8)),
            40..=47 => style.bg = Some(Color::Indexed((code - 40) as u8)),
            90..=97 => style.fg = Some(Color::Indexed((code - 90 + 8) as u8)),
            100..=107 => style.bg = Some(Color::Indexed((code - 100 + 8) as u8)),
            39 => style.fg = None,
            49 => style.bg = None,
            38 | 48 | 58 => {
                // Consume a complete extended-color group even if it is invalid;
                // its RGB numbers must never become unrelated SGR modifiers.
                let mode = values.get(i + 1).copied().flatten();
                let count = match mode {
                    Some(5) => 2,
                    Some(2) => 4,
                    _ => 1,
                };
                let color = match mode {
                    Some(5) => values
                        .get(i + 2)
                        .copied()
                        .flatten()
                        .filter(|n| *n <= 255)
                        .map(|n| Color::Indexed(n as u8)),
                    Some(2) => {
                        let rgb: Option<Vec<u8>> = (2..=4)
                            .map(|n| {
                                values
                                    .get(i + n)
                                    .copied()
                                    .flatten()
                                    .filter(|n| *n <= 255)
                                    .map(|n| n as u8)
                            })
                            .collect();
                        rgb.map(|v| Color::Rgb(v[0], v[1], v[2]))
                    }
                    _ => None,
                };
                if let Some(color) = color {
                    if code == 38 {
                        style.fg = Some(color)
                    } else if code == 48 {
                        style.bg = Some(color)
                    }
                }
                i += count;
            }
            _ => {}
        }
        i += 1;
    }
}

/// Decode styles into safe plain text before Markdown composition. Control bytes
/// have zero width; colors carry across soft wraps/newlines, not across blocks.
pub(super) fn render(input: &str, width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    let mut lines = vec![StyledLine { spans: Vec::new() }];
    let mut column = 0;
    let mut style = Style::default();
    let mut chars = input.chars().peekable();
    while let Some(mut character) = chars.next() {
        let escape = if character == '\x1b' {
            chars.next()
        } else {
            match character {
                '\u{9b}' => Some('['),
                '\u{9d}' => Some(']'),
                '\u{90}' => Some('P'),
                '\u{98}' => Some('X'),
                '\u{9e}' => Some('^'),
                '\u{9f}' => Some('_'),
                _ => None,
            }
        };
        if let Some(kind) = escape {
            match kind {
                '[' => {
                    let mut parameters = String::new();
                    let mut valid = true;
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            if c == 'm' && valid {
                                sgr(&mut style, &parameters)
                            }
                            break;
                        }
                        if parameters.len() < 256 && (c.is_ascii_digit() || c == ';') {
                            parameters.push(c)
                        } else {
                            valid = false;
                        }
                    }
                }
                ']' | 'P' | 'X' | '^' | '_' => {
                    // Strip OSC hyperlinks, clipboard/title operations, and all
                    // other control strings, including unterminated payloads.
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        if c == '\u{9c}' || (escaped && c == '\\') || (kind == ']' && c == '\x07') {
                            break;
                        }
                        escaped = c == '\x1b';
                    }
                }
                '(' | ')' | '*' | '+' | '-' | '.' | '/' | '#' => {
                    chars.next();
                }
                _ => {}
            }
            continue;
        }
        if character == '\n' {
            lines.push(StyledLine { spans: Vec::new() });
            column = 0;
            continue;
        }
        if character == '\t' {
            for _ in 0..(8 - column % 8) {
                push(&mut lines, &mut column, width, ' ', style);
            }
            continue;
        }
        if character.is_control() {
            continue;
        }
        if character.width().unwrap_or(0) > width {
            character = '�';
        }
        push(&mut lines, &mut column, width, character, style);
    }
    lines
}

fn push(
    lines: &mut Vec<StyledLine>,
    column: &mut usize,
    width: usize,
    character: char,
    style: Style,
) {
    let columns = character.width().unwrap_or(0);
    if *column + columns > width && *column > 0 {
        lines.push(StyledLine { spans: Vec::new() });
        *column = 0;
    }
    let line = lines.last_mut().unwrap();
    if let Some((text, previous)) = line.spans.last_mut()
        && previous.ansi == Some(style)
    {
        text.push(character)
    } else {
        line.spans.push((
            character.to_string(),
            LineStyle {
                ansi: Some(style),
                ..Default::default()
            },
        ));
    }
    *column += columns;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(lines: &[StyledLine]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|(s, _)| s.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn resets_foreground_and_styles_without_showing_codes() {
        let lines = render("\x1b[31;1mred\x1b[39;22m plain", 80);
        assert_eq!(text(&lines), "red plain");
        let first = lines[0].spans[0].1.ansi.unwrap();
        assert_eq!(first.fg, Some(Color::Indexed(1)));
        assert!(first.add_modifier.contains(Modifier::BOLD));
        let last = lines[0].spans[1].1.ansi.unwrap();
        assert_eq!(last.fg, None);
        assert!(last.sub_modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn supports_rgb_background_palette_underline_and_full_reset() {
        let lines = render("\x1b[38;2;1;2;3;48;5;200;4mX\x1b[0mY", 80);
        let first = lines[0].spans[0].1.ansi.unwrap();
        assert_eq!(first.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(first.bg, Some(Color::Indexed(200)));
        assert!(first.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(lines[0].spans[1].1.ansi, Some(Style::default()));
    }
    #[test]
    fn wraps_by_visible_unicode_width_and_carries_style_across_lines() {
        let lines = render("\x1b[32mab世界\nnext", 4);
        assert_eq!(text(&lines), "ab世\n界\nnext");
        assert!(
            lines
                .iter()
                .all(|line| line.spans[0].1.ansi.unwrap().fg == Some(Color::Indexed(2)))
        );
    }
    #[test]
    fn strips_terminal_commands_osc_clipboard_and_malformed_sequences() {
        let lines = render(
            "a\x1b[2Jb\x1b]52;c;SECRET\x07c\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b[999999m!\x1b[38;2;999;1;2mZ\x1b]unterminated",
            80,
        );
        assert_eq!(text(&lines), "abclink!Z");
        assert!(!text(&lines).contains('\x1b'));
        assert!(
            !lines[0]
                .spans
                .last()
                .unwrap()
                .1
                .ansi
                .unwrap()
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }
    #[test]
    fn c1_sequences_tabs_and_incomplete_csi_are_safe() {
        assert_eq!(
            text(&render("\u{9b}31mA\tB\u{9d}title\u{9c}C\x1b[", 80)),
            "A       BC"
        );
        assert_eq!(text(&render("世界", 0)), "�\n�");
    }
}
