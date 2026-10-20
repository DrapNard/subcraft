//! Persistent, user-local Activity panel. Undo state and diagnostics stay separate.

use egui::{Color32, RichText, Ui};
use subcraft_app::activity::{ActivityEntry, ActivityLevel};

use crate::{state::UiState, theme};

pub fn show(state: &mut UiState, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.heading(RichText::new("Activity").font(theme::font_strong(14.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Clear")
                .on_hover_text("Clear saved Activity history")
                .clicked()
            {
                match state.activity_log.as_mut().map(|log| log.clear()) {
                    Some(Ok(())) => state.activity_error = None,
                    Some(Err(error)) => {
                        state.activity_error =
                            Some(format!("Activity history could not be cleared: {error}"))
                    }
                    None => {}
                }
            }
        });
    });
    ui.label(
        RichText::new("Recent project, save, and export events")
            .small()
            .color(theme::TEXT_DIM),
    );
    if let Some(error) = &state.activity_error {
        ui.add_space(5.0);
        ui.colored_label(Color32::LIGHT_RED, error);
    }
    let Some(log) = state.activity_log.as_ref() else {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Activity history is unavailable in this session.")
                .color(theme::TEXT_DIM),
        );
        return;
    };
    if log.entries_newest_first().len() == 0 {
        ui.add_space(8.0);
        ui.label(RichText::new("No recent activity").color(theme::TEXT_DIM));
        return;
    }
    ui.add_space(5.0);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for entry in log.entries_newest_first() {
                row(ui, entry);
                ui.separator();
            }
        });
}

fn row(ui: &mut Ui, entry: &ActivityEntry) {
    let color = match entry.level {
        ActivityLevel::Info => theme::TEXT_DIM,
        ActivityLevel::Warning => Color32::from_rgb(235, 177, 77),
        ActivityLevel::Error => Color32::from_rgb(238, 100, 100),
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(level_label(entry.level))
                .small()
                .strong()
                .color(color),
        );
        ui.label(RichText::new(&entry.category).small().strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format_timestamp_utc(entry.timestamp_unix_ms))
                    .monospace()
                    .small()
                    .color(theme::TEXT_DIM),
            );
        });
    });
    ui.label(&entry.message);
}

fn level_label(level: ActivityLevel) -> &'static str {
    match level {
        ActivityLevel::Info => "INFO",
        ActivityLevel::Warning => "WARN",
        ActivityLevel::Error => "ERROR",
    }
}

/// Format a bounded Unix timestamp without introducing a date/time dependency.
/// UTC is explicit so the display is stable across machines and locale settings.
fn format_timestamp_utc(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let day_seconds = seconds % 86_400;
    let z = (seconds / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn timestamp_is_stable_utc_calendar_time() {
        assert_eq!(format_timestamp_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(
            format_timestamp_utc(1_700_000_000_000),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn severity_has_clear_compact_labels() {
        assert_eq!(level_label(ActivityLevel::Info), "INFO");
        assert_eq!(level_label(ActivityLevel::Warning), "WARN");
        assert_eq!(level_label(ActivityLevel::Error), "ERROR");
    }

    #[test]
    fn panel_renders_saved_entry_category_severity_and_timestamp() {
        let dir = std::env::temp_dir().join(format!(
            "subcraft-activity-ui-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut log = subcraft_app::activity::ActivityLog::load(dir.join("activity.json")).unwrap();
        log.record(ActivityLevel::Warning, "Project", "Opened recent project")
            .unwrap();
        let mut state = crate::state::UiState::new(
            subcraft_app::Session::new(subcraft_mesh::primitives::cube(2.0), 64),
            None,
        );
        state.activity_log = Some(log);
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                ..Default::default()
            },
            |ui| show(&mut state, ui),
        );
        output.textures_delta.clear();
        let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Opened recent project"));
        assert!(text.contains("Project"));
        assert!(text.contains("WARN"));
        assert!(text.contains("UTC"));
        let _ = fs::remove_dir_all(dir);
    }
}
