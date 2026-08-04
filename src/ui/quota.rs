use crate::app::App;
use crate::locale::t;
use crate::model::RateLimitInfo;
use crate::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{btop_block_active, fmt_tokens, grad_at, make_gradient, remaining_bar, styled_label};

/// Data considered "stale" when its updated_at is older than this many seconds.
const STALE_SECS: u64 = 600;

/// Fixed source order so columns stay stable across runs.
const SOURCES: &[&str] = &["claude", "codex"];

pub(crate) fn draw_quota_panel(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    draw_quota_panel_active(f, app, area, theme, false);
}

pub(crate) fn draw_quota_panel_active(
    f: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
    active: bool,
) {
    let cpu_grad = make_gradient(theme.cpu_grad.start, theme.cpu_grad.mid, theme.cpu_grad.end);

    let block = btop_block_active("quota", "²", theme.cpu_box, theme, active);
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    // Bottom summary: total tokens + rate
    let total_tokens: u64 = app.sessions.iter().map(|s| s.total_tokens()).sum();
    let rates = &app.token_rates;
    let ticks_per_min = 30usize;
    let tokens_per_min: f64 = rates.iter().rev().take(ticks_per_min).sum();

    // Split into side-by-side columns: one per known source (CLAUDE | CODEX).
    // Columns are always rendered so the panel layout stays stable even when a
    // source has no data yet.
    let num_sources = SOURCES.len() as u16;
    let col_w = inner.width / num_sources;
    let content_h = inner.height.saturating_sub(1); // reserve last row for totals

    for (i, source) in SOURCES.iter().enumerate() {
        let col_x = inner.x + (i as u16) * col_w;
        let this_w = if i as u16 == num_sources - 1 {
            inner.width - (i as u16) * col_w
        } else {
            col_w
        };
        let col_area = Rect {
            x: col_x,
            y: inner.y,
            width: this_w,
            height: content_h,
        };

        let mut limits: Vec<&RateLimitInfo> = app
            .rate_limits
            .iter()
            .filter(|r| r.source.eq_ignore_ascii_case(source))
            .collect();
        limits.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.config_root.cmp(&b.config_root))
        });
        draw_source_column(f, col_area, source, &limits, &cpu_grad, theme);
    }

    // Total tokens summary on last row (full width)
    let bottom_area = Rect {
        x: inner.x,
        y: inner.y + content_h,
        width: inner.width,
        height: 1,
    };
    let total_label = t("quota.total");
    f.render_widget(
        Paragraph::new(vec![Line::from(vec![
            Span::styled(
                format!(" {} {}", total_label, fmt_tokens(total_tokens)),
                Style::default().fg(theme.main_fg),
            ),
            Span::styled(
                format!(" {}/min", fmt_tokens(tokens_per_min as u64)),
                Style::default().fg(theme.graph_text),
            ),
        ])]),
        bottom_area,
    );
}

fn draw_source_column(
    f: &mut Frame,
    area: Rect,
    source: &str,
    limits: &[&RateLimitInfo],
    cpu_grad: &[ratatui::style::Color; 101],
    theme: &Theme,
) {
    let col_w_usize = area.width as usize;
    let bar_w = col_w_usize.saturating_sub(10).clamp(2, 8);

    let Some(rl) = limits.first().copied() else {
        let hint = if source.eq_ignore_ascii_case("claude") {
            t("quota.abtop_setup")
        } else {
            t("quota.run_codex")
        };
        let no_data = t("quota.no_data");
        let lines = vec![
            Line::from(Span::styled(
                format!(" {}", source.to_uppercase()),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  — {}", no_data),
                Style::default().fg(theme.inactive_fg),
            )),
            Line::from(Span::styled(
                format!(" {}", hint),
                Style::default().fg(theme.graph_text),
            )),
        ];
        f.render_widget(Paragraph::new(lines), area);
        return;
    };

    if limits.len() > 1 {
        draw_multi_account_column(f, area, source, limits, cpu_grad, theme);
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let is_stale = rl
        .updated_at
        .is_some_and(|ts| now.saturating_sub(ts) > STALE_SECS);

    // Stale data → dim the source name. Drops the unlabeled "Xs ago" row
    // we used to render here; keeping a distinct freshness color but no
    // explicit number is enough to signal "values may be out of date"
    // without competing with the reset countdown for the user's attention.
    let source_color = if is_stale {
        theme.inactive_fg
    } else {
        theme.title
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {}", rate_limit_title(source, rl)),
        Style::default()
            .fg(source_color)
            .add_modifier(Modifier::BOLD),
    )));

    // Reset countdown is only meaningful when the data is fresh enough
    // that the reported `resets_at` is still in the future. Stale sources
    // get the bar (the % is approximately correct) but no countdown row.
    let show_reset = !is_stale;

    if let Some(used_pct) = rl.five_hour_pct {
        let remaining = (100.0 - used_pct).clamp(0.0, 100.0);
        let reset = if show_reset {
            rl.five_hour_resets_at
                .map(format_reset_time)
                .unwrap_or_default()
        } else {
            String::new()
        };
        let c = grad_at(cpu_grad, used_pct);
        let label_5h = format_window_label(rl.five_hour_window_minutes, t("quota.5h"));
        let mut s = vec![styled_label(
            format!(" {}", label_5h).as_str(),
            theme.graph_text,
        )];
        s.extend(remaining_bar(remaining, bar_w, cpu_grad, theme.meter_bg));
        s.push(Span::styled(
            format!(" {:>3.0}%", remaining),
            Style::default().fg(c),
        ));
        lines.push(Line::from(s));
        // Always reserve the row so both columns line up vertically;
        // when there's nothing meaningful to show (stale source or the
        // cached reset moment is past), render it blank.
        lines.push(Line::from(Span::styled(
            if reset.is_empty() {
                String::new()
            } else {
                format!("  {}", reset)
            },
            Style::default().fg(theme.graph_text),
        )));
    }
    if let Some(used_pct) = rl.seven_day_pct {
        let remaining = (100.0 - used_pct).clamp(0.0, 100.0);
        let reset = if show_reset {
            rl.seven_day_resets_at
                .map(format_reset_time)
                .unwrap_or_default()
        } else {
            String::new()
        };
        let c = grad_at(cpu_grad, used_pct);
        let label_7d = format_window_label(rl.seven_day_window_minutes, t("quota.7d"));
        let mut s = vec![styled_label(
            format!(" {}", label_7d).as_str(),
            theme.graph_text,
        )];
        s.extend(remaining_bar(remaining, bar_w, cpu_grad, theme.meter_bg));
        s.push(Span::styled(
            format!(" {:>3.0}%", remaining),
            Style::default().fg(c),
        ));
        lines.push(Line::from(s));
        // Always reserve the row so both columns line up vertically;
        // when there's nothing meaningful to show (stale source or the
        // cached reset moment is past), render it blank.
        lines.push(Line::from(Span::styled(
            if reset.is_empty() {
                String::new()
            } else {
                format!("  {}", reset)
            },
            Style::default().fg(theme.graph_text),
        )));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_multi_account_column(
    f: &mut Frame,
    area: Rect,
    source: &str,
    limits: &[&RateLimitInfo],
    cpu_grad: &[ratatui::style::Color; 101],
    theme: &Theme,
) {
    let max_accounts = area.height.saturating_sub(1) as usize;
    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", source.to_uppercase()),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    ))];

    for rl in limits.iter().take(max_accounts) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let stale = rl
            .updated_at
            .is_some_and(|timestamp| now.saturating_sub(timestamp) > STALE_SECS);
        let mut spans = vec![Span::styled(
            format!(" {:<7}", account_profile_label(source, rl)),
            Style::default()
                .fg(if stale {
                    theme.inactive_fg
                } else {
                    theme.main_fg
                })
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(compact_window_spans(
            rl.five_hour_pct,
            rl.five_hour_window_minutes,
            t("quota.5h"),
            cpu_grad,
            theme,
        ));
        spans.extend(compact_window_spans(
            rl.seven_day_pct,
            rl.seven_day_window_minutes,
            t("quota.7d"),
            cpu_grad,
            theme,
        ));
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn compact_window_spans(
    used_pct: Option<f64>,
    window_minutes: Option<u64>,
    fallback: String,
    cpu_grad: &[ratatui::style::Color; 101],
    theme: &Theme,
) -> Vec<Span<'static>> {
    let label = format_window_label(window_minutes, fallback);
    let Some(used_pct) = used_pct else {
        return vec![Span::styled(
            format!(" {label} —"),
            Style::default().fg(theme.inactive_fg),
        )];
    };
    let remaining = (100.0 - used_pct).clamp(0.0, 100.0);
    vec![Span::styled(
        format!(" {label} {:>3.0}%", remaining),
        Style::default().fg(grad_at(cpu_grad, used_pct)),
    )]
}

fn rate_limit_title(source: &str, info: &RateLimitInfo) -> String {
    let source = source.to_uppercase();
    format!("{source} · {}", account_profile_label(&source, info))
}

fn account_profile_label(source: &str, info: &RateLimitInfo) -> String {
    let Some(name) = std::path::Path::new(&info.config_root)
        .file_name()
        .and_then(|value| value.to_str())
    else {
        return "account".to_string();
    };
    let name = name.trim_start_matches('.');
    let profile = name
        .strip_prefix(&format!("{}-", source.to_ascii_lowercase()))
        .unwrap_or(name);
    let profile = if profile.eq_ignore_ascii_case(source) {
        "default"
    } else {
        profile
    };
    profile.to_string()
}

fn format_window_label(window_minutes: Option<u64>, fallback: String) -> String {
    let Some(minutes) = window_minutes else {
        return fallback;
    };

    if minutes == 0 {
        fallback
    } else if minutes % (24 * 60) == 0 {
        format!("{}{}", minutes / (24 * 60), t("time.d"))
    } else if minutes % 60 == 0 {
        format!("{}{}", minutes / 60, t("time.h"))
    } else {
        format!("{}{}", minutes, t("time.m"))
    }
}

/// Format a reset timestamp as a human countdown labeled "in X" so the
/// row reads as a time-until-reset. Returns an empty string when the
/// reset is already in the past — the actual next reset depends on the
/// window length which the caller doesn't track, and showing a "now"
/// sentinel was misleading on stale sources where the window had
/// already rolled over multiple times. Callers skip the row when this
/// returns empty.
pub(crate) fn format_reset_time(reset_ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if reset_ts <= now {
        return String::new();
    }
    let diff = reset_ts - now;
    let prefix = t("quota.in");
    if diff < 60 {
        format!("{} {}{}", prefix, diff, t("time.s"))
    } else if diff < 3600 {
        format!("{} {}{}", prefix, diff / 60, t("time.m"))
    } else if diff < 86400 {
        let h = diff / 3600;
        let m = (diff % 3600) / 60;
        format!("{} {}{} {}{}", prefix, h, t("time.h"), m, t("time.m"))
    } else {
        let d = diff / 86400;
        let h = (diff % 86400) / 3600;
        format!("{} {}{} {}{}", prefix, d, t("time.d"), h, t("time.h"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PanelVisibility;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn account_profile_labels_distinguish_auth_roots() {
        let codex_2001 = RateLimitInfo {
            source: "codex".to_string(),
            config_root: "~/.codex-2001".to_string(),
            ..Default::default()
        };
        let codex_default = RateLimitInfo {
            source: "codex".to_string(),
            config_root: "~/.codex".to_string(),
            ..Default::default()
        };
        let claude_alex = RateLimitInfo {
            source: "claude".to_string(),
            config_root: "~/.claude-alex".to_string(),
            ..Default::default()
        };

        assert_eq!(account_profile_label("codex", &codex_2001), "2001");
        assert_eq!(account_profile_label("codex", &codex_default), "default");
        assert_eq!(account_profile_label("claude", &claude_alex), "alex");
        assert_eq!(rate_limit_title("codex", &codex_2001), "CODEX · 2001");
    }

    #[test]
    fn quota_panel_renders_each_auth_profile() {
        let mut app = App::new_with_config(Theme::default(), &[], PanelVisibility::default());
        app.rate_limits = [
            ("claude", "~/.claude-alex", 55.0),
            ("claude", "~/.claude-chi", 26.0),
            ("codex", "~/.codex-2001", 38.0),
            ("codex", "~/.codex-3001", 39.0),
        ]
        .into_iter()
        .map(|(source, config_root, used)| RateLimitInfo {
            source: source.to_string(),
            config_root: config_root.to_string(),
            five_hour_pct: Some(used),
            five_hour_window_minutes: Some(300),
            seven_day_pct: Some(used + 10.0),
            seven_day_window_minutes: Some(10_080),
            updated_at: Some(u64::MAX),
            ..Default::default()
        })
        .collect();

        let backend = TestBackend::new(64, 9);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_quota_panel(frame, &app, Rect::new(0, 0, 64, 9), &app.theme);
            })
            .unwrap();
        let text = format!("{}", terminal.backend());

        for expected in ["CLAUDE", "alex", "chi", "CODEX", "2001", "3001"] {
            assert!(
                text.contains(expected),
                "quota panel should show {expected}\n{text}"
            );
        }
    }
}
