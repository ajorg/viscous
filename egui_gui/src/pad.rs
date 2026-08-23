//! Reading an actual game controller.
//!
//! The mapping — which stick does what, which button stands for which preset —
//! lives in [`viscous::gamepad`] and is exercised without hardware. This is the
//! other half: the thin layer that finds a controller on this machine and reads
//! where its controls are, which is the only part a plugged-in controller is
//! needed to check.
//!
//! Nothing here has to be captured or claimed. A controller is not a keyboard:
//! the operating system hands its state to whoever asks, so what reaches this
//! window doesn't depend on which window is in front.

use gilrs::{Axis, Button, Gilrs};
use viscous::gamepad::{Buttons, Pad};

/// Something to read a controller from — the ones plugged in, or a stand-in
/// held in a fixed position for a test.
pub trait Controller {
    /// Where the controls are now, or `None` when there is no controller to
    /// read: an empty socket asks for nothing, rather than asking for rest.
    fn poll(&mut self) -> Option<Pad>;
}

/// The controllers plugged into this machine.
pub struct Attached(Gilrs);

impl Attached {
    /// Starts watching for controllers, or reports that this platform has no
    /// way to — in which case the window simply has no controller in it, which
    /// is what a machine without one looks like anyway.
    ///
    /// Called once, before any controller is plugged in: they are noticed as
    /// they arrive, so the one found in a drawer mid-service still works.
    pub fn open() -> Option<Self> {
        Gilrs::new().ok().map(Self)
    }
}

impl Controller for Attached {
    fn poll(&mut self) -> Option<Pad> {
        // Draining the event queue is what brings the remembered state up to
        // date; the events themselves say what changed, which is a question
        // this program never has to ask.
        while self.0.next_event().is_some() {}
        // The first controller connected, of however many: this window drives
        // one camera, and a second stick on it would only fight the first.
        self.0.gamepads().next().map(|(_, pad)| read(&pad))
    }
}

/// One controller's own controls, whatever is being asked.
trait Controls {
    fn axis(&self, axis: Axis) -> f32;
    fn pressed(&self, button: Button) -> bool;
    /// How far in an analogue button is, from `0.0` to `1.0`.
    fn pressure(&self, button: Button) -> f32;
}

impl Controls for gilrs::Gamepad<'_> {
    fn axis(&self, axis: Axis) -> f32 {
        self.value(axis)
    }

    fn pressed(&self, button: Button) -> bool {
        self.is_pressed(button)
    }

    fn pressure(&self, button: Button) -> f32 {
        match self.button_data(button) {
            Some(data) => data.value(),
            // A trigger the driver reports as a plain button, or one that
            // hasn't moved since the controller was plugged in.
            None if self.is_pressed(button) => 1.0,
            None => 0.0,
        }
    }
}

/// Where every control on one controller is.
///
/// The triggers are read as buttons because that is what they arrive as — an
/// analogue button, with the pressure still on it — rather than as the Z axes,
/// which not every driver fills in.
fn read(controls: &impl Controls) -> Pad {
    Pad {
        left_stick: (
            controls.axis(Axis::LeftStickX),
            controls.axis(Axis::LeftStickY),
        ),
        right_stick: (
            controls.axis(Axis::RightStickX),
            controls.axis(Axis::RightStickY),
        ),
        left_trigger: controls.pressure(Button::LeftTrigger2),
        right_trigger: controls.pressure(Button::RightTrigger2),
        buttons: Buttons {
            south: controls.pressed(Button::South),
            east: controls.pressed(Button::East),
            west: controls.pressed(Button::West),
            north: controls.pressed(Button::North),
            // The shoulders, where `LeftTrigger` is the bumper above the
            // trigger proper — the layout's names, not this crate's.
            left_bumper: controls.pressed(Button::LeftTrigger),
            right_bumper: controls.pressed(Button::RightTrigger),
            start: controls.pressed(Button::Start),
            back: controls.pressed(Button::Select),
            left_stick: controls.pressed(Button::LeftThumb),
            right_stick: controls.pressed(Button::RightThumb),
            dpad_up: controls.pressed(Button::DPadUp),
            dpad_down: controls.pressed(Button::DPadDown),
            dpad_left: controls.pressed(Button::DPadLeft),
            dpad_right: controls.pressed(Button::DPadRight),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A controller held still in whatever position a test needs.
    #[derive(Default)]
    struct Bench {
        axes: Vec<(Axis, f32)>,
        pressures: Vec<(Button, f32)>,
        down: Vec<Button>,
    }

    impl Controls for Bench {
        fn axis(&self, axis: Axis) -> f32 {
            self.axes
                .iter()
                .find(|(which, _)| *which == axis)
                .map_or(0.0, |(_, value)| *value)
        }

        fn pressed(&self, button: Button) -> bool {
            self.down.contains(&button)
        }

        fn pressure(&self, button: Button) -> f32 {
            self.pressures
                .iter()
                .find(|(which, _)| *which == button)
                .map_or(0.0, |(_, value)| *value)
        }
    }

    fn resting() -> Bench {
        Bench::default()
    }

    #[test]
    fn a_controller_nobody_is_touching_reads_as_one() {
        assert_eq!(read(&resting()), Pad::default());
    }

    #[test]
    fn each_stick_reads_as_its_own_stick() {
        let pad = read(&Bench {
            axes: vec![
                (Axis::LeftStickX, 0.5),
                (Axis::LeftStickY, -0.25),
                (Axis::RightStickX, 1.0),
                (Axis::RightStickY, 0.75),
            ],
            ..resting()
        });

        assert_eq!(pad.left_stick, (0.5, -0.25));
        assert_eq!(pad.right_stick, (1.0, 0.75));
    }

    #[test]
    fn the_triggers_read_as_far_as_they_are_pushed() {
        let pad = read(&Bench {
            pressures: vec![(Button::LeftTrigger2, 0.3), (Button::RightTrigger2, 0.9)],
            ..resting()
        });

        assert_eq!(pad.left_trigger, 0.3);
        assert_eq!(pad.right_trigger, 0.9);
    }

    /// Whether one particular button is down, as [`viscous::gamepad`] reports
    /// it — the other side of the translation this module is.
    type Down = fn(&Buttons) -> bool;

    #[test]
    fn each_button_reads_as_its_own_button() {
        let buttons: [(Button, Down); 14] = [
            (Button::South, |buttons| buttons.south),
            (Button::East, |buttons| buttons.east),
            (Button::West, |buttons| buttons.west),
            (Button::North, |buttons| buttons.north),
            (Button::LeftTrigger, |buttons| buttons.left_bumper),
            (Button::RightTrigger, |buttons| buttons.right_bumper),
            (Button::Start, |buttons| buttons.start),
            (Button::Select, |buttons| buttons.back),
            (Button::LeftThumb, |buttons| buttons.left_stick),
            (Button::RightThumb, |buttons| buttons.right_stick),
            (Button::DPadUp, |buttons| buttons.dpad_up),
            (Button::DPadDown, |buttons| buttons.dpad_down),
            (Button::DPadLeft, |buttons| buttons.dpad_left),
            (Button::DPadRight, |buttons| buttons.dpad_right),
        ];

        for (button, down) in buttons {
            let pad = read(&Bench {
                down: vec![button],
                ..resting()
            });

            assert!(down(&pad.buttons), "{button:?} should read as pressed");
            assert_eq!(
                buttons
                    .iter()
                    .filter(|(_, down)| down(&pad.buttons))
                    .count(),
                1,
                "{button:?} should be the only button pressed"
            );
        }
    }
}
