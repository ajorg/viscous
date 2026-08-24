//! The switch that arms the Mark column, drawn as a padlock.
//!
//! Painted rather than typed: neither of the fonts this window ships with has
//! a padlock in it (`🔒` and `🔓` fall back to a blank box), and a hand-painted
//! one costs less than carrying a font for two glyphs — the pad and the rockers
//! are painted here for the same reason.
//!
//! The shackle says which way the lock is: seated in the body when marking is
//! locked, sprung open when it isn't. That is the state, not the action — see
//! [`tooltip`], which is where the action is spelled out, since a padlock alone
//! can be read either way.
//!
//! Drawn the way the mechanism actually works. A modern shackle has a long leg
//! and a short one: the long leg — the heel — lives deep in a bore and is held
//! there by a retaining groove, and the short one, the toe, is what the latch
//! catches. Releasing the latch lets a spring drive the whole bar *straight up*
//! until the toe is clear of the body while the heel is still buried. The
//! swivel that follows turns about the heel's own axis, which is vertical, so
//! it sweeps out of the plane this is drawn in and is left out: the lift is the
//! part of the motion a front view can tell the truth about.

use egui::{Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

/// How much of the button's height the padlock is drawn at, leaving the rest
/// as the margin any button keeps around what it says.
const FILL: f32 = 0.68;

/// The padlock's parts, as fractions of that height: the body's width and
/// height, the shackle's radius and the thickness of its wire, and how far the
/// shackle's shoulders stand above the body when it is shut.
///
/// These and [`LIFT`] add up to the whole — lift, shoulder, shackle and half
/// the wire it is stroked with, on top of the body — so an open lock fills the
/// height it is given, and a shut one leaves exactly the headroom its shackle
/// will rise into.
const BODY: Vec2 = vec2(0.74, 0.45);
const SHOULDER: f32 = 0.06;
const SHACKLE: f32 = 0.24;
const WIRE: f32 = 0.10;

/// How far into the body each leg reaches when the lock is shut: the heel,
/// which stays captive, and the toe, which the latch catches.
///
/// Hidden behind the body either way — the body is painted over them — but they
/// are what makes the lift mean something. [`LIFT`] has to be longer than the
/// toe, so the toe comes clear, and shorter than the heel, so the heel doesn't.
const HEEL: f32 = 0.32;
const TOE: f32 = 0.03;

/// How far the shackle springs when the lock opens: straight up, the whole bar,
/// far enough that the gap under the toe reads at the size a line of text is.
const LIFT: f32 = 0.22;

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
    let center = pos2(body.center().x, body.top() - unit(SHOULDER));

    // One bent bar of unequal legs: the long heel, the bend over the top, the
    // short toe. Built the same both ways, because a shackle is a piece of
    // steel and steel neither shortens nor grows a leg when a lock opens.
    let mut shackle = vec![pos2(center.x - radius, body.top() + unit(HEEL))];
    shackle.extend((0..=ARC_STEPS).map(|step| {
        let turn = std::f32::consts::PI * (1.0 + step as f32 / ARC_STEPS as f32);
        pos2(
            center.x + radius * turn.cos(),
            center.y + radius * turn.sin(),
        )
    }));
    shackle.push(pos2(center.x + radius, body.top() + unit(TOE)));

    // Open, the spring drives the whole bar straight up. Nothing turns: the
    // swivel a padlock has is about the heel's own axis and sweeps out of this
    // plane. The toe rises clear of the body and the heel stays in it, which is
    // the whole of what the picture has to say.
    if !locked {
        for point in &mut shackle {
            point.y -= unit(LIFT);
        }
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

    /// The two ends of the bar: the long heel and the short toe.
    fn ends(locked: bool) -> (egui::Pos2, egui::Pos2) {
        let bar = shackle(locked);
        (bar[0], *bar.last().expect("the bar has two ends"))
    }

    #[test]
    fn a_shut_lock_has_both_legs_down_inside_the_body() {
        let (heel, toe) = ends(true);
        let top = body(true).top();

        assert!(
            heel.y > top && toe.y > top,
            "shut, both legs are in their bores: heel {heel:?}, toe {toe:?}, top {top}"
        );
        assert!(
            heel.y > toe.y,
            "the heel is the long leg — it reaches deeper than the toe: \
             {heel:?} against {toe:?}"
        );
    }

    /// The length of every straight run of the bar, in order — the shackle's
    /// dimensions, which are the thing that cannot change.
    fn segments(locked: bool) -> Vec<f32> {
        shackle(locked)
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .collect()
    }

    #[test]
    fn opening_the_lock_moves_the_shackle_without_resizing_it() {
        let (shut, open) = (segments(true), segments(false));

        assert_eq!(
            shut.len(),
            open.len(),
            "the bar should not gain or lose parts"
        );
        for (shut, open) in shut.iter().zip(&open) {
            assert!(
                (shut - open).abs() < 0.01,
                "a shackle is steel: {shut} became {open}"
            );
        }
    }

    #[test]
    fn opening_it_lifts_the_bar_straight_up_and_turns_nothing() {
        let (shut, open) = (shackle(true), shackle(false));

        for (shut, open) in shut.iter().zip(&open) {
            assert!(
                (shut.x - open.x).abs() < 0.01,
                "a shackle rises in its bores; it does not swing in this plane: \
                 {shut:?} became {open:?}"
            );
            assert!(
                open.y < shut.y,
                "and it rises rather than sinks: {shut:?} became {open:?}"
            );
        }
    }

    #[test]
    fn opening_it_frees_the_toe_and_keeps_the_heel() {
        let (heel, toe) = ends(false);
        let top = body(false).top();

        assert!(
            toe.y < top - 1.0,
            "the toe should stand clear of the body with a gap that reads: \
             {toe:?} against a top at {top}"
        );
        assert!(
            heel.y > top,
            "the heel stays captive, or the shackle would be in your hand: \
             {heel:?} against a top at {top}"
        );
    }

    #[test]
    fn the_open_lock_fills_the_height_it_is_given() {
        // The open one, because that is the taller of the two: a shut lock
        // leaves the headroom its shackle is about to rise into, rather than
        // the whole lock shifting down the button when it opens.
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        let open = shackle(false);
        let drawn = Rect::from_min_max(
            pos2(
                0.0,
                open.iter().map(|point| point.y).fold(f32::MAX, f32::min),
            ),
            pos2(0.0, body(false).bottom()),
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
    fn the_body_stays_where_it_is_whichever_way_the_lock_is() {
        assert_eq!(
            body(true),
            body(false),
            "the shackle moves; the lock it hangs on does not"
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
