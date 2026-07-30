//! The pan/tilt drive surface: the same distance-controls-speed idea as the
//! D70 Commander's jog area, built directly on egui's `Sense::click_and_drag`
//! and `Painter` rather than a bespoke widget trait — the idiomatic way to
//! build a custom interactive control in egui, with no external widget crate
//! needed.

use std::f32::consts::PI;

use egui::{Sense, Stroke, Ui, Vec2, vec2};
use viscous::pan_tilt::{self, Velocity};

/// The velocity a puck offset asks for, taking the pad's radius as full
/// deflection and flipping screen y (which grows downward) so that dragging
/// up tilts up.
pub fn velocity_for_offset(offset: Vec2, radius: f32) -> Velocity {
    pan_tilt::velocity_from_axes(offset.x / radius, -offset.y / radius)
}

/// Clamps an offset from the pad's center to its edge, so dragging off the
/// pad asks for full speed rather than something beyond it.
fn clamp_to_radius(offset: Vec2, radius: f32) -> Vec2 {
    if offset.length() > radius {
        offset.normalized() * radius
    } else {
        offset
    }
}

/// Draws the pad and returns the velocity the pointer is asking for, which is
/// [`Velocity::STOP`] whenever nothing is holding it.
///
/// The puck's position is derived from the live pointer state rather than
/// remembered between frames: "let go" is exactly "no pointer button held on
/// this widget", which is also precisely when the camera should stop.
pub fn pan_tilt_pad(ui: &mut Ui, size: f32) -> Velocity {
    let radius = size / 2.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let center = rect.center();

    let offset = match response.interact_pointer_pos() {
        Some(pos) if response.is_pointer_button_down_on() => clamp_to_radius(pos - center, radius),
        _ => Vec2::ZERO,
    };

    let visuals = ui.visuals();
    let track_color = visuals.extreme_bg_color;
    let tick_color = visuals.weak_text_color();
    let accent = visuals.selection.bg_fill;

    let painter = ui.painter();
    painter.circle_filled(center, radius, track_color);

    let tick_stroke = Stroke::new(1.0, tick_color);
    for i in 0..8 {
        let angle = i as f32 * PI / 4.0;
        let dir = vec2(angle.cos(), -angle.sin());
        painter.line_segment(
            [center + dir * radius * 0.85, center + dir * radius * 0.98],
            tick_stroke,
        );
    }

    let puck_center = center + offset;
    if offset.length() > 0.5 {
        painter.line_segment(
            [center, puck_center],
            Stroke::new(2.0, accent.gamma_multiply(0.6)),
        );
    }
    painter.circle_filled(center, 4.0, accent.gamma_multiply(0.6));
    painter.circle_filled(puck_center, 14.0, accent);

    velocity_for_offset(offset, radius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::command::PanTiltDirection;

    const RADIUS: f32 = 120.0;

    #[test]
    fn a_centered_puck_stops() {
        assert!(velocity_for_offset(Vec2::ZERO, RADIUS).is_stop());
    }

    #[test]
    fn dragging_up_the_screen_tilts_up() {
        // Screen y grows downward, so "up" is a negative offset.
        assert_eq!(
            velocity_for_offset(vec2(0.0, -RADIUS), RADIUS).direction,
            PanTiltDirection::Up
        );
        assert_eq!(
            velocity_for_offset(vec2(0.0, RADIUS), RADIUS).direction,
            PanTiltDirection::Down
        );
    }

    #[test]
    fn dragging_sideways_pans_that_way() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, 0.0), RADIUS).direction,
            PanTiltDirection::Right
        );
        assert_eq!(
            velocity_for_offset(vec2(-RADIUS, 0.0), RADIUS).direction,
            PanTiltDirection::Left
        );
    }

    #[test]
    fn dragging_diagonally_drives_both_axes() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, -RADIUS), RADIUS).direction,
            PanTiltDirection::UpRight
        );
    }

    #[test]
    fn the_edge_of_the_pad_is_full_speed() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, 0.0), RADIUS).pan_speed,
            viscous::pan_tilt::MAX_PAN_SPEED
        );
    }

    #[test]
    fn speed_scales_with_distance_from_the_center() {
        let near = velocity_for_offset(vec2(30.0, 0.0), RADIUS).pan_speed;
        let far = velocity_for_offset(vec2(110.0, 0.0), RADIUS).pan_speed;
        assert!(
            near < far,
            "a further drag should ask for a faster pan ({near} vs {far})"
        );
    }

    #[test]
    fn dragging_past_the_edge_is_clamped_to_it() {
        assert_eq!(clamp_to_radius(vec2(500.0, 0.0), RADIUS), vec2(RADIUS, 0.0));
        assert_eq!(clamp_to_radius(vec2(10.0, 0.0), RADIUS), vec2(10.0, 0.0));
    }
}
