//! Drawing the setup and settings screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{AccountEditor, App, Modal, Row, Section, Tone};
use super::form::{FieldKind, Form};

fn tone_style(t: Tone) -> Style {
    match t {
        Tone::Normal => Style::default(),
        Tone::Dim => Style::default().fg(Color::DarkGray),
        Tone::Action => Style::default().fg(Color::Cyan),
        Tone::Warn => Style::default().fg(Color::Yellow),
    }
}

fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub(crate) fn render(app: &App, f: &mut Frame) {
    let [header, body, status, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" second-brain {} ", app.mode),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", app.home),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        header,
    );
    let [nav, detail] =
        Layout::horizontal([Constraint::Length(20), Constraint::Min(20)]).areas(body);
    render_nav(app, f, nav);
    render_section(app, f, detail);
    if let Some(s) = &app.status {
        f.render_widget(
            Paragraph::new(format!(" {}", s.text)).style(if s.error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            }),
            status,
        );
    }
    f.render_widget(
        Paragraph::new(format!(" {}", help_line(app))).style(Style::default().fg(Color::DarkGray)),
        help,
    );
    for m in &app.modals {
        render_modal(f, m, body);
    }
}

fn render_nav(app: &App, f: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = Section::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mark = match app.done(*s) {
                Some(true) => Span::styled("✓", Style::default().fg(Color::Green)),
                Some(false) => Span::styled("·", Style::default().fg(Color::Yellow)),
                None => Span::raw(" "),
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} {:<14}", i + 1, s.title())),
                mark,
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.section));
    f.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Sections "))
            .highlight_style(selected()),
        area,
        &mut state,
    );
}

fn row_items(rows: &[Row]) -> Vec<ListItem<'static>> {
    rows.iter()
        .map(|r| ListItem::new(Span::styled(format!(" {}", r.text), tone_style(r.tone))))
        .collect()
}

fn render_section(app: &App, f: &mut Frame, area: Rect) {
    let s = app.current();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", s.title()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let intro = s.intro();
    let [top, list] = Layout::vertical([
        Constraint::Length(intro.len() as u16 + 1),
        Constraint::Min(1),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(
            intro
                .iter()
                .map(|l| Line::raw(format!(" {l}")))
                .collect::<Vec<_>>(),
        )
        .style(Style::default().fg(Color::DarkGray)),
        top,
    );
    let rows = app.rows(s);
    let mut state = ListState::default().with_selected(Some(app.cursor()));
    let focused = app.modals.is_empty();
    f.render_stateful_widget(
        List::new(row_items(&rows)).highlight_style(if focused {
            selected()
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }),
        list,
        &mut state,
    );
}

/// The key help line for the focused element.
fn help_line(app: &App) -> String {
    match app.modals.last() {
        Some(Modal::Form(..)) => {
            "Tab/↑↓ move · Space toggle · Enter next/submit · Esc cancel".into()
        }
        Some(Modal::Pick { .. }) => "↑↓ move · Enter choose · Esc cancel".into(),
        Some(Modal::Confirm { .. }) => "y yes · n no".into(),
        Some(Modal::Account(_)) => {
            "↑↓ move · Enter edit/toggle · x remove the key (use the default) · Esc close".into()
        }
        None => {
            let rows = app.rows(app.current());
            let row = rows.get(app.cursor());
            let mut parts = vec!["↑↓ move".to_string()];
            if let Some(a) = row.and_then(|r| r.primary.as_ref()) {
                parts.push(format!("Enter {}", a.label()));
            }
            if let Some(a) = row.and_then(|r| r.secondary.as_ref()) {
                parts.push(format!("x {}", a.label()));
            }
            parts.push("Tab/1-8 section".into());
            parts.push("q quit".into());
            parts.join(" · ")
        }
    }
}

/// A rectangle of at most `w` x `h`, centered in `area`.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn render_modal(f: &mut Frame, m: &Modal, body: Rect) {
    match m {
        Modal::Form(form, _) => render_form(f, form, body),
        Modal::Pick {
            title,
            options,
            cursor,
            ..
        } => {
            let w = options
                .iter()
                .map(|o| Span::raw(o.as_str()).width() as u16 + 6)
                .max()
                .unwrap_or(20)
                .max(title.len() as u16 + 6);
            let area = centered(body, w, options.len() as u16 + 2);
            f.render_widget(Clear, area);
            let mut state = ListState::default().with_selected(Some(*cursor));
            f.render_stateful_widget(
                List::new(
                    options
                        .iter()
                        .map(|o| ListItem::new(format!(" {o}")))
                        .collect::<Vec<_>>(),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {title} ")),
                )
                .highlight_style(selected()),
                area,
                &mut state,
            );
        }
        Modal::Confirm { text, .. } => {
            let area = centered(body, Span::raw(text.as_str()).width() as u16 + 6, 5);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(vec![Line::raw(""), Line::raw(format!(" {text}"))])
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Confirm (y/n) "),
                    )
                    .wrap(Wrap { trim: false }),
                area,
            );
        }
        Modal::Account(ed) => render_account(f, ed, body),
    }
}

fn render_account(f: &mut Frame, ed: &AccountEditor, body: Rect) {
    let area = centered(
        body,
        body.width.saturating_sub(4),
        body.height.saturating_sub(2),
    );
    f.render_widget(Clear, area);
    let key_w = ed.rows.iter().map(|r| r.key.len()).max().unwrap_or(10);
    let items: Vec<ListItem> = ed
        .rows
        .iter()
        .map(|r| {
            let style = if r.default {
                tone_style(Tone::Dim)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(
                format!(
                    " {:<key_w$}  {}{}",
                    r.key,
                    super::app::show_value(&r.value),
                    if r.default { "  (default)" } else { "" }
                ),
                style,
            ))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(ed.cursor));
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Account config: {} ", ed.title)),
            )
            .highlight_style(selected()),
        area,
        &mut state,
    );
}

fn render_form(f: &mut Frame, form: &Form, body: Rect) {
    let label_w = form
        .fields
        .iter()
        .map(|x| Span::raw(x.label.as_str()).width())
        .max()
        .unwrap_or(8);
    let help = form
        .fields
        .get(form.focus)
        .and_then(|x| x.help.clone())
        .unwrap_or_default();
    let height = form.notes.len() as u16 + form.fields.len() as u16 + 7;
    let area = centered(body, 76, height);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", form.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = form
        .notes
        .iter()
        .map(|n| Line::styled(format!(" {n}"), Style::default().fg(Color::DarkGray)))
        .collect();
    if !form.notes.is_empty() {
        lines.push(Line::raw(""));
    }
    let value_x = inner.x + label_w as u16 + 4;
    let value_w = inner.width.saturating_sub(label_w as u16 + 5) as usize;
    let mut cursor = None;
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let value = match &field.kind {
            FieldKind::Text(input) => {
                let (shown, x) = input.view(value_w);
                if focused {
                    cursor = Some(Position {
                        x: value_x + x as u16,
                        y: inner.y + lines.len() as u16,
                    });
                }
                shown
            }
            FieldKind::Toggle(on) => (if *on { "[x] yes" } else { "[ ] no" }).to_string(),
        };
        let label = Span::styled(
            format!(" {:<label_w$}   ", field.label),
            if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        );
        let value = Span::styled(
            value,
            if focused {
                Style::default().add_modifier(Modifier::UNDERLINED)
            } else {
                Style::default().fg(Color::Cyan)
            },
        );
        lines.push(Line::from(vec![label, value]));
    }
    lines.push(Line::raw(""));
    let submit = format!(" [ {} ] ", form.submit);
    lines.push(Line::from(Span::styled(
        submit,
        if form.focus >= form.fields.len() {
            selected()
        } else {
            Style::default().fg(Color::Cyan)
        },
    )));
    lines.push(Line::styled(
        format!(" {help}"),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(lines), inner);
    if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
}
