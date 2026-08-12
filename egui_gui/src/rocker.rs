//! A rocker: a control that rests at its centre, drives whichever way it's
//! pushed, and drives harder the further it's pushed.
//!
//! The one-axis form of the pan/tilt pad, and the control a camera operator
//! already has under their thumb: a zoom rocker springs back to the middle
//! when you let go, which is exactly when the camera should stop.

use egui::{Sense, Stroke, Ui, pos2, vec2};

/// How long the track is, as a multiple of the height of a line of body text,
/// and how thick — measured in the text beside it like everything else here.
const TRACK_LENGTHS: f32 = 8.0;
const TRACK_THICKNESS: f32 = 0.45;

/// The knob's diameter, as a multiple of the track's thickness.
const KNOB: f32 = 1.6;

/// What one rocker is marked with.
pub struct Marks<'a> {
    /// What this rocker drives, in a word.
    pub label: &'a str,
    /// The marks at the left and right ends of the travel, in that order.
    pub ends: (&'a str, &'a str),
    /// What each end does, spelled out for whoever hasn't met the marks.
    pub tooltips: (&'a str, &'a str),
}

/// Draws a rocker and returns how far it's being pushed, from -1 at the left
/// end to 1 at the right — and 0 whenever nothing is holding it, or whenever
/// it isn't `live`, which draws it dead and says `why` on hover.
///
/// Like the pad, the knob's position comes from the live pointer rather than
/// from anything remembered between frames: letting go *is* the spring that
/// returns it to the middle.
pub fn rocker(ui: &mut Ui, marks: &Marks<'_>, live: bool, why: &str) -> f32 {
    ui.add_enabled_ui(live, |ui| {
        ui.horizontal(|ui| {
            ui.label(marks.label);
            end(ui, marks.ends.0, marks.tooltips.0, why);
            let deflection = track(ui, marks.label, live, why);
            end(ui, marks.ends.1, marks.tooltips.1, why);
            deflection
        })
        .inner
    })
    .inner
}

/// One end mark, which says what that end does — or why it currently does
/// nothing.
fn end(ui: &mut Ui, mark: &str, tooltip: &str, why: &str) {
    ui.label(mark)
        .on_hover_text(tooltip)
        .on_disabled_hover_text(why);
}

/// The track and its knob, without the marks around them.
fn track(ui: &mut Ui, label: &str, live: bool, why: &str) -> f32 {
    let text = ui.text_style_height(&egui::TextStyle::Body);
    let thickness = text * TRACK_THICKNESS;
    let sense = if live {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(text * TRACK_LENGTHS, text), sense);

    let center = rect.center();
    let reach = rect.width() / 2.0;
    let deflection = match response.interact_pointer_pos() {
        Some(pos) if response.is_pointer_button_down_on() => {
            ((pos.x - center.x) / reach).clamp(-1.0, 1.0)
        }
        _ => 0.0,
    };

    let visuals = ui.visuals();
    let knob_color = if live {
        visuals.selection.bg_fill
    } else {
        visuals.weak_text_color()
    };
    let hairline = Stroke::new(
        visuals.widgets.noninteractive.bg_stroke.width,
        visuals.weak_text_color(),
    );
    let painter = ui.painter();

    let groove = egui::Rect::from_center_size(center, vec2(rect.width(), thickness));
    painter.rect_filled(groove, thickness / 2.0, visuals.extreme_bg_color);
    // The rest position, marked so it's clear there is one to come back to.
    painter.line_segment(
        [
            pos2(center.x, groove.top()),
            pos2(center.x, groove.bottom()),
        ],
        hairline,
    );
    painter.circle_filled(
        pos2(center.x + deflection * reach, center.y),
        thickness * KNOB / 2.0,
        knob_color,
    );

    // A hand-painted control is invisible to a screen reader unless it says
    // what it is; a slider is exactly what this behaves like, so it says so —
    // and that is also how a test finds it.
    response.widget_info(|| egui::WidgetInfo::slider(live, f64::from(deflection), label));
    response.on_hover_text(why);
    deflection
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use std::cell::Cell;

    fn zoom_marks() -> Marks<'static> {
        Marks {
            label: "Zoom",
            ends: ("W", "T"),
            tooltips: ("Wider", "Tighter"),
        }
    }

    /// What a rocker reports after a frame in which the pointer does whatever
    /// `push` does to it.
    fn deflection_after(live: bool, push: impl Fn(&mut Harness<'_, ()>, egui::Rect)) -> f32 {
        let reported = Cell::new(f32::NAN);
        let mut harness = Harness::new_ui(|ui| {
            reported.set(rocker(ui, &zoom_marks(), live, "not just now"));
        });
        harness.run();

        // The track says it is a slider named for what it drives, so it is
        // found the same way a user's screen reader would find it.
        let track = harness
            .get_all_by_role(egui::accesskit::Role::Slider)
            .next()
            .expect("the rocker should offer a track to push")
            .rect();
        push(&mut harness, track);
        harness.run();
        reported.get()
    }

    #[test]
    fn a_rocker_nobody_is_holding_rests_in_the_middle() {
        assert_eq!(deflection_after(true, |_, _| {}), 0.0);
    }

    #[test]
    fn pushing_a_rocker_towards_an_end_asks_to_go_that_way() {
        let right = deflection_after(true, |harness, track| {
            harness.drag_at(track.right_center());
        });
        let left = deflection_after(true, |harness, track| {
            harness.drag_at(track.left_center());
        });

        assert!(right > 0.9, "the right end is full deflection: {right}");
        assert!(left < -0.9, "the left end is full deflection: {left}");
    }

    #[test]
    fn pushing_further_from_the_middle_asks_for_more() {
        let near = deflection_after(true, |harness, track| {
            harness.drag_at(track.center() + vec2(track.width() * 0.2, 0.0));
        });
        let far = deflection_after(true, |harness, track| {
            harness.drag_at(track.center() + vec2(track.width() * 0.4, 0.0));
        });

        assert!(
            near < far,
            "further out should ask for more ({near}, {far})"
        );
    }

    #[test]
    fn a_rocker_that_is_not_live_stays_put_however_it_is_pushed() {
        let pushed = deflection_after(false, |harness, track| {
            harness.drag_at(track.right_center());
        });

        assert_eq!(pushed, 0.0);
    }
}
