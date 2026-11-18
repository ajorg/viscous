//! The pan/tilt drive surface: the same distance-controls-speed idea as the
//! D70 Commander's jog area, built directly on egui's `Sense::click_and_drag`
//! and `Painter` rather than a bespoke widget trait — the idiomatic way to
//! build a custom interactive control in egui, with no external widget crate
//! needed.

use std::{f32::consts::PI, sync::OnceLock};

use egui::{Sense, Stroke, TextStyle, Ui, Vec2, vec2};
use viscous::pan_tilt::{self, PadLayout, Velocity};

/// The smallest the pad is drawn, as a multiple of the height of a line of
/// body text.
///
/// The pad has no size of its own to be right about: what matters is that it
/// stays at least a comfortable hand's worth of travel next to the controls
/// around it, so its floor is measured in the same text the rest of the window
/// is laid out in and follows the user's interface scaling along with
/// everything else. Given a larger window it takes the room, since a bigger
/// pad is a finer aim.
const PAD_TEXT_HEIGHTS: f32 = 16.0;

/// The puck and the dot marking the centre, as fractions of the pad's radius,
/// so a larger pad gets a proportionally larger puck rather than the same one
/// adrift in more space.
const PUCK_RADIUS: f32 = 0.12;
const CENTER_RADIUS: f32 = 0.033;

/// How far the lines marking where each speed begins stand out from the track
/// they are drawn on, as a share of the way from the track's colour to the
/// colour of text.
///
/// A shade of the track rather than a colour of their own: a little lighter
/// than it in a dark scheme, a little darker in a light one. They are there to
/// be found by an eye that goes looking, and a grid drawn any louder would be
/// the first thing seen on a control whose point is the puck.
const BAND_LINE_STRENGTH: f32 = 0.12;

/// The stroke the lines marking where each speed begins are drawn with, on
/// this pad and on the rockers under it.
pub fn band_line(visuals: &egui::Visuals) -> Stroke {
    Stroke::new(
        visuals.widgets.noninteractive.bg_stroke.width,
        visuals
            .extreme_bg_color
            .lerp_to_gamma(visuals.text_color(), BAND_LINE_STRENGTH),
    )
}

/// Where the eight direction ticks start and end, as fractions of the radius.
const TICK_INNER: f32 = 0.85;
const TICK_OUTER: f32 = 0.98;

/// Draws the part of a circle around `center` between two directions, in
/// degrees anticlockwise from straight right.
fn arc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    (from, to): (f32, f32),
    stroke: Stroke,
) {
    // A degree a segment, which no eye tells from a curve at any size this
    // pad is drawn.
    let segments = ((to - from).abs().ceil() as usize).max(1);
    let points = (0..=segments)
        .map(|segment| {
            let angle = (from + (to - from) * segment as f32 / segments as f32).to_radians();
            center + vec2(angle.cos(), -angle.sin()) * radius
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
}

/// The velocity a puck offset asks for, taking the pad's radius as full
/// deflection and flipping screen y (which grows downward) so that dragging
/// up tilts up.
pub fn velocity_for_offset(offset: Vec2, radius: f32, layout: PadLayout) -> Velocity {
    layout.velocity(offset.x / radius, -offset.y / radius)
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

/// Where the puck sits to show a game controller's stick: the same offset the
/// pointer would be holding it at to ask for the same drive, with screen y
/// flipped back, since a stick pushed up reads positive.
///
/// Clamped like a drag, so a stick pushed into its corners — where the two axes
/// together reach past the circle — stays on the pad it belongs to.
fn offset_for_stick(stick: Vec2, radius: f32) -> Vec2 {
    clamp_to_radius(vec2(stick.x, -stick.y) * radius, radius)
}

/// The smallest pad the given `ui` should draw: a fixed number of lines of its
/// own body text, so it scales with the interface rather than with the display.
pub fn least_pad_size(ui: &Ui) -> f32 {
    ui.text_style_height(&TextStyle::Body) * PAD_TEXT_HEIGHTS
}

/// Draws the pad and returns the velocity the pointer is asking for, which is
/// [`Velocity::STOP`] whenever nothing is holding it — or whenever it isn't
/// `live`, which draws it dead and says `why` on hover.
///
/// The puck's position is derived from the live pointer state rather than
/// remembered between frames: "let go" is exactly "no pointer button held on
/// this widget", which is also precisely when the camera should stop.
///
/// With no pointer on it the puck shows `stick` instead — where a game
/// controller's stick is being held, as fractions of full deflection, positive
/// being right and up. The pad is then a display: one place to see the pan and
/// tilt being asked for, whichever hand is asking. Where the stick is shown
/// even when the pad isn't `live`, in the dead colour the rest of it is drawn
/// in: that a stick is answering is worth seeing whatever the camera is doing
/// about it.
///
/// `layout` says how a push becomes pan and tilt, and `speed_lines` draws the
/// edges of the regions it cuts the pad into.
pub fn pan_tilt_pad(
    ui: &mut Ui,
    size: f32,
    live: bool,
    why: &str,
    stick: Vec2,
    layout: PadLayout,
    speed_lines: bool,
) -> Velocity {
    let radius = size / 2.0;
    let sense = if live {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), sense);
    let center = rect.center();

    let dragged = match response.interact_pointer_pos() {
        Some(pos) if response.is_pointer_button_down_on() => {
            Some(clamp_to_radius(pos - center, radius))
        }
        _ => None,
    };
    let offset = dragged.unwrap_or_else(|| offset_for_stick(stick, radius));

    let visuals = ui.visuals();
    let track_color = visuals.extreme_bg_color;
    let accent = if live {
        visuals.selection.bg_fill
    } else {
        visuals.weak_text_color()
    };
    // The theme's own hairline, so the ticks match the separators and frame
    // edges drawn beside them at whatever width it draws those.
    let tick_stroke = Stroke::new(
        visuals.widgets.noninteractive.bg_stroke.width,
        visuals.weak_text_color(),
    );

    let band_stroke = band_line(visuals);

    let painter = ui.painter();
    painter.circle_filled(center, radius, track_color);

    // For whoever has asked to see it: the pad cut into its regions, each one
    // a stretch of it where the camera is asked for the same pan speed and
    // the same tilt speed wherever in it the puck is. A ring wherever being
    // further out starts to mean faster, the innermost being where the pad
    // starts asking for anything at all; and across each ring, a line
    // wherever the direction starts to mean a different share between the
    // two.
    if speed_lines && layout == PadLayout::EqualAreas {
        for region in pan_tilt::regions::regions() {
            let corners = region
                .outline
                .iter()
                .map(|(right, up)| center + vec2(*right, -*up) * radius)
                .collect();
            painter.add(egui::Shape::closed_line(corners, band_stroke));
        }
        // Whatever of the regions reaches into the deadzone asks for
        // nothing there, so it isn't drawn as if it did.
        let resting = viscous::deflection::DEADZONE * radius;
        painter.circle_filled(center, resting, track_color);
        painter.circle_stroke(center, resting, band_stroke);
    } else if speed_lines {
        // The same every frame, and found by trying the whole pad.
        static SPLITS: OnceLock<Vec<Vec<f32>>> = OnceLock::new();
        static EDGES: OnceLock<Vec<Vec<(f32, f32)>>> = OnceLock::new();
        let splits = SPLITS.get_or_init(pan_tilt::split_changes);
        let edges = EDGES.get_or_init(pan_tilt::ring_edges);

        let rings = pan_tilt::ring_starts();
        let ends = rings.iter().skip(1).copied().chain([1.0]);
        for (((start, end), changes), stretches) in rings.iter().zip(ends).zip(splits).zip(edges) {
            for (from, to) in stretches {
                // Found for the quarter between right and up, like the
                // changes below; the other three are that one mirrored.
                for (a, b) in [
                    (*from, *to),
                    (180.0 - to, 180.0 - from),
                    (180.0 + from, 180.0 + to),
                    (360.0 - to, 360.0 - from),
                ] {
                    arc(painter, center, start * radius, (a, b), band_stroke);
                }
            }
            for degrees in changes {
                // Found for the quarter between right and up; the other
                // three are that one mirrored.
                for mirrored in [*degrees, 180.0 - degrees, 180.0 + degrees, 360.0 - degrees] {
                    let angle = mirrored.to_radians();
                    let dir = vec2(angle.cos(), -angle.sin());
                    painter.line_segment(
                        [
                            center + dir * (start * radius),
                            center + dir * (end * radius),
                        ],
                        band_stroke,
                    );
                }
            }
        }
    }

    for i in 0..8 {
        let angle = i as f32 * PI / 4.0;
        let dir = vec2(angle.cos(), -angle.sin());
        painter.line_segment(
            [
                center + dir * radius * TICK_INNER,
                center + dir * radius * TICK_OUTER,
            ],
            tick_stroke,
        );
    }

    let puck_radius = radius * PUCK_RADIUS;
    let center_radius = radius * CENTER_RADIUS;
    let puck_center = center + offset;
    // Only worth drawing once the puck has left the centre dot it would
    // otherwise be hidden under.
    if offset.length() > center_radius {
        painter.line_segment(
            [center, puck_center],
            Stroke::new(center_radius * 2.0, accent.gamma_multiply(0.6)),
        );
    }
    painter.circle_filled(center, center_radius, accent.gamma_multiply(0.6));
    painter.circle_filled(puck_center, puck_radius, accent);

    // A hand-painted control is invisible to a screen reader unless it says
    // what it is — and this is also how a test finds it.
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, live, "Pan and tilt"));
    // Only while it is dead: `why` says what is stopping this pad working, and
    // one that is working has no such thing to report. (Said here rather than
    // through egui's own disabled-hover text because the pad is painted by
    // hand — nothing above it has been told it is disabled.)
    if !live {
        response.on_hover_text(why);
    }

    // Only the pointer's own drag is asked for here; what the stick asks for
    // reaches the camera by its own path, and returning it too would have the
    // pad ask for it a second time.
    velocity_for_offset(dragged.unwrap_or(Vec2::ZERO), radius, layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontId;
    use egui_kittest::{Harness, kittest::Queryable};
    use grafton_visca::command::PanTiltDirection;
    use std::cell::Cell;

    const RADIUS: f32 = 120.0;

    /// The smallest pad a window whose body text is `height` tall would draw.
    fn pad_size_with_body_height(height: f32) -> f32 {
        let measured = Cell::new(0.0);
        Harness::new_ui(|ui| {
            ui.style_mut()
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(height));
            measured.set(least_pad_size(ui));
        })
        .run();
        measured.get()
    }

    #[test]
    fn the_pad_is_measured_in_the_text_beside_it() {
        let small = pad_size_with_body_height(10.0);
        let large = pad_size_with_body_height(20.0);

        assert!(
            (large / small - 2.0).abs() < 0.1,
            "twice the text should ask for twice the pad ({small} then {large})"
        );
    }

    #[test]
    fn a_centered_puck_stops() {
        assert!(velocity_for_offset(Vec2::ZERO, RADIUS, PadLayout::Rings).is_stop());
        assert!(velocity_for_offset(Vec2::ZERO, RADIUS, PadLayout::EqualAreas).is_stop());
    }

    #[test]
    fn dragging_up_the_screen_tilts_up() {
        // Screen y grows downward, so "up" is a negative offset.
        assert_eq!(
            velocity_for_offset(vec2(0.0, -RADIUS), RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::Up
        );
        assert_eq!(
            velocity_for_offset(vec2(0.0, RADIUS), RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::Down
        );
    }

    #[test]
    fn dragging_sideways_pans_that_way() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, 0.0), RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::Right
        );
        assert_eq!(
            velocity_for_offset(vec2(-RADIUS, 0.0), RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::Left
        );
    }

    #[test]
    fn dragging_diagonally_drives_both_axes() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, -RADIUS), RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::UpRight
        );
    }

    #[test]
    fn the_edge_of_the_pad_is_full_speed() {
        assert_eq!(
            velocity_for_offset(vec2(RADIUS, 0.0), RADIUS, PadLayout::Rings).pan_speed,
            *viscous::pan_tilt::PAN_SPEEDS.end()
        );
    }

    #[test]
    fn speed_scales_with_distance_from_the_center() {
        let near = velocity_for_offset(vec2(30.0, 0.0), RADIUS, PadLayout::Rings).pan_speed;
        let far = velocity_for_offset(vec2(110.0, 0.0), RADIUS, PadLayout::Rings).pan_speed;
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

    #[test]
    fn the_puck_shows_a_stick_where_a_drag_asking_the_same_would_be() {
        // A stick pushed up is drawn up the screen, which is downward in the
        // numbers, and the drive it stands for is the one that drag asks for.
        let up = offset_for_stick(vec2(0.0, 1.0), RADIUS);

        assert_eq!(up, vec2(0.0, -RADIUS));
        assert_eq!(
            velocity_for_offset(up, RADIUS, PadLayout::Rings).direction,
            PanTiltDirection::Up
        );
        assert_eq!(
            offset_for_stick(vec2(-1.0, 0.0), RADIUS),
            vec2(-RADIUS, 0.0)
        );
    }

    #[test]
    fn a_stick_in_its_corner_still_lands_on_the_pad() {
        let corner = offset_for_stick(vec2(1.0, 1.0), RADIUS);

        assert!(
            (corner.length() - RADIUS).abs() < 0.001,
            "the corner should sit on the edge, not past it: {corner:?}"
        );
    }

    /// Whether "not just now" — the reason a dead pad gives — is on screen
    /// after the pointer has come to rest on a pad that is `live`.
    fn hovering_the_pad_explains_itself(live: bool) -> bool {
        let mut harness = Harness::new_ui(move |ui| {
            pan_tilt_pad(
                ui,
                200.0,
                live,
                "not just now",
                Vec2::ZERO,
                PadLayout::Rings,
                false,
            );
        });
        harness.run();

        harness
            .get_all_by_label("Pan and tilt")
            .next()
            .expect("the pad should say what it is")
            .hover();
        harness.run();

        harness.query_by_label("not just now").is_some()
    }

    #[test]
    fn a_working_pad_does_not_give_a_reason_it_is_not_working() {
        assert!(
            !hovering_the_pad_explains_itself(true),
            "a pad that drives the camera shouldn't say why it can't"
        );
    }

    #[test]
    fn a_dead_pad_says_why_when_the_pointer_lands_on_it() {
        assert!(
            hovering_the_pad_explains_itself(false),
            "the pad is the first thing reached for, so it has to explain itself"
        );
    }

    #[test]
    fn a_stick_shown_on_the_pad_is_not_the_pad_asking_for_it() {
        // The stick reaches the camera by its own path; a pad that also asked
        // for what it is only showing would be asking twice.
        let asked = Cell::new(Velocity::STOP);
        Harness::new_ui(|ui| {
            asked.set(pan_tilt_pad(
                ui,
                200.0,
                true,
                "",
                vec2(1.0, 1.0),
                PadLayout::EqualAreas,
                true,
            ));
        })
        .run();

        assert!(asked.get().is_stop());
    }
}
