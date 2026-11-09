use super::{LineStyle, th};

/// A line with styled spans from markdown rendering
pub(super) struct StyledLine {
    pub(super) spans: Vec<(String, LineStyle)>,
}

/// Render markdown text to styled lines for TUI display
pub(super) fn render_markdown_to_lines(input: &str, max_width: usize) -> Vec<StyledLine> {
    let doc = crate::markdown::layout::LayoutEngine::render(input, max_width);
    doc.lines
        .into_iter()
        // Drop fence-only lines — TUI signals code blocks via color instead.
        // Empty lines are kept (they're blank spacers, not fences).
        .filter(|line| line.runs.is_empty() || line.runs.iter().any(|r| !r.attrs.code_fence))
        .map(|line| StyledLine {
            spans: line
                .runs
                .into_iter()
                .filter(|run| !run.attrs.link_url && !run.attrs.heading_marker)
                .map(|run| (run.text, attrs_to_line_style(&run.attrs)))
                .collect(),
        })
        .collect()
}

/// Formatting off is safe literal text; formatting on combines Markdown and ANSI.
pub(super) fn render_text_to_lines(input: &str, width: usize, formatting: bool) -> Vec<StyledLine> {
    if !formatting {
        let mut lines = super::ansi::render(input, width);
        for line in &mut lines {
            for (_, style) in &mut line.spans {
                style.ansi = None;
            }
        }
        return lines;
    }
    if !super::ansi::has_ansi(input) {
        return render_markdown_to_lines(input, width);
    }

    // Decode first so escape bytes cannot become Markdown syntax or layout width.
    // Keep byte provenance so generated prefixes and repeated/link text cannot
    // accidentally pick up colors from a different occurrence in the source.
    let decoded = super::ansi::render(input, usize::MAX);
    let mut plain = String::new();
    let mut colors = Vec::new();
    for (index, line) in decoded.into_iter().enumerate() {
        if index > 0 {
            plain.push('\n');
        }
        for (text, style) in line.spans {
            let start = plain.len();
            plain.push_str(&text);
            colors.push((start..plain.len(), style.ansi.unwrap_or_default()));
        }
    }
    let color_at = |offset: usize| {
        let index = colors.partition_point(|(range, _)| range.end <= offset);
        colors
            .get(index)
            .filter(|(range, _)| range.contains(&offset))
            .map(|(_, style)| *style)
    };
    crate::markdown::layout::LayoutEngine::render_with_source(&plain, width)
        .lines
        .into_iter()
        .filter(|line| line.runs.is_empty() || line.runs.iter().any(|r| !r.attrs.code_fence))
        .map(|line| {
            let mut spans = Vec::new();
            for run in line
                .runs
                .into_iter()
                .filter(|run| !run.attrs.link_url && !run.attrs.heading_marker)
            {
                let base = attrs_to_line_style(&run.attrs);
                if let Some(range) = run.source_range {
                    if plain.get(range.clone()) == Some(run.text.as_str()) {
                        let mut grouped: Vec<(String, LineStyle)> = Vec::new();
                        for (index, character) in run.text.char_indices() {
                            let ansi = color_at(range.start + index);
                            if let Some((text, style)) = grouped.last_mut()
                                && style.ansi == ansi
                            {
                                text.push(character);
                            } else {
                                grouped.push((
                                    character.to_string(),
                                    LineStyle {
                                        ansi,
                                        ..base.clone()
                                    },
                                ));
                            }
                        }
                        spans.extend(grouped);
                    } else {
                        spans.push((
                            run.text,
                            LineStyle {
                                ansi: color_at(range.start),
                                ..base
                            },
                        ));
                    }
                } else {
                    spans.push((run.text, base));
                }
            }
            StyledLine { spans }
        })
        .collect()
}

fn attrs_to_line_style(attrs: &crate::markdown::layout::Attrs) -> LineStyle {
    let fg = if let Some(rgb) = attrs.fg {
        Some(rgb)
    } else if attrs.code_block_lang.is_some() || attrs.code {
        Some(th().code_color)
    } else if attrs.quote {
        Some(th().green)
    } else if attrs.link {
        Some(th().blue)
    } else if attrs.heading {
        Some(th().heading)
    } else {
        None
    };
    LineStyle {
        bold: attrs.bold || attrs.heading,
        italic: attrs.italic,
        dimmed: attrs.dimmed || attrs.strikethrough,
        fg,
        ..Default::default()
    }
}

/// Give thinking prose its muted color without overwriting Markdown accents.
pub(super) fn apply_thinking_style(styled_lines: Vec<StyledLine>) -> Vec<StyledLine> {
    styled_lines
        .into_iter()
        .map(|line| StyledLine {
            spans: line
                .spans
                .into_iter()
                .map(|(text, mut style)| {
                    style.italic = true;
                    style.fg.get_or_insert(th().thinking_text);
                    (text, style)
                })
                .collect(),
        })
        .collect()
}
