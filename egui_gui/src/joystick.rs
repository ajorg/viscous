//! The pan/tilt drag surface: the same distance-controls-speed idea as the
//! D70 Commander's jog area, built directly on egui's `Sense::drag` +
//! `Painter` rather than a bespoke widget trait — the idiomatic way to build
//! a custom interactive control in egui, with no external widget crate
//! needed.

use std::f32::consts::PI;

use egui::{Response, Sense, Stroke, Ui, Vec2, vec2};
use viscous::pan_tilt::NudgeDirection;

/// The nudge size, in degrees, at the deadzone edge and at full radius.
const MIN_NUDGE_DEGREES: f64 = 1.0;
const MAX_NUDGE_DEGREES: f64 = 10.0;

/// Drags closer to center than this fraction of the radius send nothing —
/// otherwise an intended tap-to-recenter would jitter out a tiny nudge.
const DEADZONE_FRACTION: f32 = 0.12;

/// Maps a drag offset (screen coordinates, y grows downward) to the nearest
/// of eight compass directions, with "up" meaning visually up.
pub fn direction_of(offset: Vec2) -> NudgeDirection {
    let angle = (-offset.y).atan2(offset.x);
    let sector = (angle / (PI / 4.0)).round() as i32;
    const ORDER: [NudgeDirection; 8] = [
        NudgeDirection::Right,
        NudgeDirection::UpRight,
        NudgeDirection::Up,
        NudgeDirection::UpLeft,
        NudgeDirection::Left,
        NudgeDirection::DownLeft,
        NudgeDirection::Down,
        NudgeDirection::DownRight,
    ];
    ORDER[sector.rem_euclid(8) as usize]
}

/// Maps a drag distance to a nudge size in degrees, `None` inside the
/// deadzone.
pub fn degrees_for_distance(distance: f32, radius: f32) -> Option<f64> {
    let deadzone = radius * DEADZONE_FRACTION;
    if distance <= deadzone {
        return None;
    }
    let t = ((distance - deadzone) / (radius - deadzone)).clamp(0.0, 1.0) as f64;
    Some(MIN_NUDGE_DEGREES + t * (MAX_NUDGE_DEGREES - MIN_NUDGE_DEGREES))
}

/// Draws the pad and updates `drag_offset` from the current pointer
/// interaction. Returns the direction/degrees to nudge this frame, if the
/// drag is currently held outside the deadzone.
pub fn pan_tilt_pad(
    ui: &mut Ui,
    size: f32,
    drag_offset: &mut Vec2,
) -> (Response, Option<(NudgeDirection, f64)>) {
    let radius = size / 2.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let center = rect.center();

    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let mut offset = pos - center;
        if offset.length() > radius {
            offset = offset.normalized() * radius;
        }
        *drag_offset = offset;
    }
    if response.drag_stopped() {
        *drag_offset = Vec2::ZERO;
    }

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

    let puck_center = center + *drag_offset;
    if drag_offset.length() > 0.5 {
        painter.line_segment(
            [center, puck_center],
            Stroke::new(2.0, accent.gamma_multiply(0.6)),
        );
    }
    painter.circle_filled(center, 4.0, accent.gamma_multiply(0.6));
    painter.circle_filled(puck_center, 14.0, accent);

    let nudge = degrees_for_distance(drag_offset.length(), radius)
        .map(|degrees| (direction_of(*drag_offset), degrees));

    (response, nudge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_of_right_is_zero_offset_angle() {
        assert_eq!(direction_of(vec2(10.0, 0.0)), NudgeDirection::Right);
    }

    #[test]
    fn direction_of_negative_y_is_up() {
        assert_eq!(direction_of(vec2(0.0, -10.0)), NudgeDirection::Up);
    }

    #[test]
    fn direction_of_diagonal_up_right() {
        assert_eq!(direction_of(vec2(10.0, -10.0)), NudgeDirection::UpRight);
    }

    #[test]
    fn direction_of_diagonal_down_left() {
        assert_eq!(direction_of(vec2(-10.0, 10.0)), NudgeDirection::DownLeft);
    }

    #[test]
    fn degrees_for_distance_is_none_inside_deadzone() {
        assert_eq!(degrees_for_distance(1.0, 120.0), None);
    }

    #[test]
    fn degrees_for_distance_scales_with_distance() {
        let near = degrees_for_distance(20.0, 120.0).unwrap();
        let far = degrees_for_distance(110.0, 120.0).unwrap();
        assert!(
            far > near,
            "farther drags should nudge more than closer ones"
        );
    }

    #[test]
    fn degrees_for_distance_caps_at_the_radius() {
        assert_eq!(degrees_for_distance(120.0, 120.0), Some(10.0));
    }
}
