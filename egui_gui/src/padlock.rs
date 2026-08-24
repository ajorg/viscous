//! The switch that arms the Mark column, drawn as a padlock.
//!
//! Painted rather than typed: neither of the fonts this window ships with has
//! a padlock in it (`🔒` and `🔓` fall back to a blank box), and a hand-painted
//! one costs less than carrying a font for two glyphs — the pad and the rockers
//! are painted here for the same reason.
//!
//! The shackle says which way the lock is: closed and sitting in the body when
//! marking is locked, swung open when it isn't. That is the state, not the
//! action — see [`tooltip`], which is where the action is spelled out, since a
//! padlock alone can be read either way.

use egui::{Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

/// How much of the button's height the padlock is drawn at, leaving the rest
/// as the margin any button keeps around what it says.
const FILL: f32 = 0.68;

/// The padlock's parts, as fractions of that height: the body's width and
/// height, the shackle's radius and the thickness of its wire, and how far the
/// shackle's shoulders stand above the body.
///
/// The four heights add up to the whole — body, shoulder, shackle and half the
/// wire it is stroked with — so the lock fills the height it is given rather
/// than sitting somewhere inside it.
const BODY: Vec2 = vec2(0.85, 0.58);
const SHOULDER: f32 = 0.06;
const SHACKLE: f32 = 0.30;
const WIRE: f32 = 0.12;

/// How many straight segments the shackle's arc is drawn with. Enough that the
/// curve reads as one at the size a line of text is.
const ARC_STEPS: usize = 12;

/// What the lock says about itself, in words, on hover.
///
/// Both halves matter: the state the picture is already showing, and the thing
/// a click will do. A padlock on its own is genuinely ambiguous about which of
/// the two it means, and this is the cheapest place to settle it.
pub fn tooltip(locked: bool) -> &'static str {
    if locked {
        "Marking is locked, so a stray click can't overwrite a preset. \
         Click to allow storing shots."
    } else {
        "Marking is on: the Mark buttons will overwrite presets. \
         Click to lock them again."
    }
}

/// Draws the lock at exactly `size`, and says whether it was clicked.
///
/// Sized by its caller rather than by its contents so that it can be given the
/// width of the column it heads: a button that decided its own width would sit
/// over the Mark buttons without lining up with them.
pub fn lock_button(ui: &mut Ui, size: Vec2, locked: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let visuals = ui.style().interact_selectable(&response, !locked);
    let painter = ui.painter();
    painter.rect(
        rect.expand(visuals.expansion),
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    for shape in shapes(rect, locked, visuals.fg_stroke.color) {
        painter.add(shape);
    }

    // A painted control is invisible to a screen reader unless it says what it
    // is. Named for what it turns on rather than for the picture, so what is
    // read out is the same word the column is known by — and that is also how
    // a test finds it.
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, !locked, "Marking")
    });
    response.on_hover_text(tooltip(locked)).clicked()
}

/// The padlock itself: a body with a shackle standing on it, closed or open.
///
/// Drawn to the height it is given and centred in the width, so the button can
/// be as wide as the column it heads without stretching the picture on it.
fn shapes(rect: Rect, locked: bool, color: egui::Color32) -> Vec<Shape> {
    let height = rect.height() * FILL;
    let unit = |fraction: f32| height * fraction;

    let foot = rect.center().y + height / 2.0;
    let body = Rect::from_min_max(
        pos2(rect.center().x - unit(BODY.x) / 2.0, foot - unit(BODY.y)),
        pos2(rect.center().x + unit(BODY.x) / 2.0, foot),
    );
    let wire = Stroke::new(unit(WIRE), color);
    let radius = unit(SHACKLE);
    // The closed shackle stands in the middle of the body; the open one has
    // swung up and to the right, so its far leg no longer reaches.
    let center = pos2(
        body.center().x + if locked { 0.0 } else { radius },
        body.top() - unit(SHOULDER),
    );

    let mut shackle: Vec<_> = (0..=ARC_STEPS)
        .map(|step| {
            let turn = std::f32::consts::PI * (1.0 + step as f32 / ARC_STEPS as f32);
            pos2(
                center.x + radius * turn.cos(),
                center.y + radius * turn.sin(),
            )
        })
        .collect();
    // The near leg drops into the body either way; the far one only when the
    // lock is shut, which is the whole of the difference between the two.
    shackle.insert(0, pos2(center.x - radius, body.top()));
    if locked {
        shackle.push(pos2(center.x + radius, body.top()));
    }

    vec![
        Shape::Path(egui::epaint::PathShape::line(shackle, wire)),
        Shape::rect_filled(body, unit(0.08), color),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the shackle's wire runs, at the size a preset row would draw it.
    fn shackle(locked: bool) -> Vec<egui::Pos2> {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        match shapes(rect, locked, egui::Color32::WHITE).remove(0) {
            Shape::Path(path) => path.points,
            other => panic!("the shackle should be a path, not {other:?}"),
        }
    }

    /// The body of the lock, which both states share.
    fn body(locked: bool) -> Rect {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        match shapes(rect, locked, egui::Color32::WHITE).remove(1) {
            Shape::Rect(shape) => shape.rect,
            other => panic!("the body should be a rectangle, not {other:?}"),
        }
    }

    #[test]
    fn the_shackle_stands_inside_the_body_it_shuts_into() {
        let shut = shackle(true);
        let body = body(true);
        let (left, right) = (
            shut.iter().map(|point| point.x).fold(f32::MAX, f32::min),
            shut.iter().map(|point| point.x).fold(f32::MIN, f32::max),
        );

        assert!(
            left > body.left() && right < body.right(),
            "a shackle wider than the lock it shuts is not a padlock: \
             {left}..{right} against {body:?}"
        );
    }

    #[test]
    fn a_shut_lock_has_both_legs_in_the_body() {
        let shut = shackle(true);
        let top = body(true).top();

        let legs = shut
            .iter()
            .filter(|point| (point.y - top).abs() < 0.01)
            .count();
        assert_eq!(legs, 2, "both ends should reach the body: {shut:?}");
    }

    #[test]
    fn an_open_lock_has_one_leg_free_and_the_shackle_swung_aside() {
        let open = shackle(false);
        let top = body(false).top();

        let legs = open
            .iter()
            .filter(|point| (point.y - top).abs() < 0.01)
            .count();
        assert_eq!(legs, 1, "only the near leg should reach the body: {open:?}");
        assert!(
            open.last().expect("the arc should have an end").y < top,
            "the far end should hang above the body: {open:?}"
        );
        assert!(
            open.iter().any(|point| point.x > body(false).right()),
            "the shackle should have swung clear of the body: {open:?}"
        );
    }

    #[test]
    fn the_lock_fills_the_height_it_is_given() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        let shut = shackle(true);
        let drawn = Rect::from_min_max(
            pos2(
                0.0,
                shut.iter().map(|point| point.y).fold(f32::MAX, f32::min),
            ),
            pos2(0.0, body(true).bottom()),
        );

        assert!(
            drawn.height() > rect.height() * 0.5,
            "a lock lost inside its button is a lock nobody can read: {drawn:?}"
        );
        assert!(
            drawn.height() < rect.height(),
            "it should keep the margin a button keeps: {drawn:?}"
        );
        assert!(
            (drawn.center().y - rect.center().y).abs() < 1.0,
            "and sit in the middle of it: {drawn:?}"
        );
    }

    #[test]
    fn both_locks_are_drawn_at_the_same_size_in_the_same_place() {
        assert_eq!(body(true), body(false));

        let (shut, open) = (shackle(true), shackle(false));
        let highest = |points: &[egui::Pos2]| {
            points
                .iter()
                .map(|point| point.y)
                .fold(f32::INFINITY, f32::min)
        };
        assert!(
            (highest(&shut) - highest(&open)).abs() < 0.01,
            "the shackle should swing, not jump: {shut:?} against {open:?}"
        );
    }

    #[test]
    fn the_hover_text_says_both_where_it_is_and_what_a_click_does() {
        for locked in [true, false] {
            let text = tooltip(locked);
            assert!(
                text.contains("Click to"),
                "the action should be spelled out: {text}"
            );
        }
        assert_ne!(tooltip(true), tooltip(false));
    }
}
