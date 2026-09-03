//! Live camera state for the info panel: what the camera reports about
//! itself beyond the one-time version reply.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::FocusMode,
    transport::{BlockingTransport, HasTransportConfig},
};
use serde::{Deserialize, Serialize};

/// A snapshot of what the camera says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraState {
    /// Whether the camera is awake.
    pub power_on: bool,
    /// Where the camera is pointing, zoomed and focused — or `None` when it
    /// has nothing to say about that, which is what standby and the seconds
    /// after being woken both look like.
    pub lens: Option<Lens>,
}

/// Where the camera is pointing, zoomed and focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lens {
    /// Where the lens is aimed.
    pub position: Position,
    /// Whether the camera is focusing automatically, in which case a manual
    /// focus drive won't hold.
    pub auto_focus: bool,
}

/// Everything it takes to reproduce a shot: where the lens is aimed, how far
/// in, and what it is focused on.
///
/// Held in the camera's own units rather than in degrees. Degrees would have
/// to be converted through the profile's units-per-degree, and the profile
/// this program connects with is `GenericVisca`, whose figure is a
/// conservative stand-in for cameras it has never met — so a position written
/// down in degrees and read back would land somewhere else. See
/// [`crate::nudge::nudge`], which declines the same conversion for the same
/// reason.
///
/// Written to the config file, so it is our own plain struct rather than
/// grafton-visca's newtypes: what a saved shot looks like on disk shouldn't be
/// a dependency's business.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Pan, in raw camera units.
    pub pan: i16,
    /// Tilt, in raw camera units.
    pub tilt: i16,
    /// Zoom, in raw camera units.
    pub zoom: u16,
    /// Focus, in raw camera units.
    pub focus: u16,
}

/// Queries a connected camera for its current state.
///
/// Power comes first and stands on its own: **what the camera says about its
/// power is never thrown away because some other inquiry failed.** An operator
/// who has just put the camera into standby needs the switch to say so, and it
/// can't if one all-or-nothing snapshot fails on the lens — the switch would
/// go on offering to do the thing that has already been done.
///
/// So the lens is only asked about once the camera says it is awake, and a
/// lens that won't answer is reported as a lens that won't answer. Cameras
/// differ in how they refuse: some return "not executable", some say nothing
/// and time out, and a camera part-way through waking up does whichever it
/// feels like. None of those are worth telling the operator apart, and keying
/// on any of them would be keying on the model rather than on the protocol.
pub fn query_state<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<CameraState, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let power_on = camera.power_state()?;
    Ok(CameraState {
        power_on,
        // Nothing to ask a sleeping camera: it has parked its lens, and every
        // inquiry is a round trip spent being refused.
        lens: power_on.then(|| query_lens(camera).ok()).flatten(),
    })
}

fn query_lens<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<Lens, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    Ok(Lens {
        position: query_position(camera)?,
        auto_focus: camera.focus_mode()? == FocusMode::Auto,
    })
}

/// Asks the camera where it is aimed, without the rest of the state snapshot.
///
/// For the moments when the answer is worth more than usual: the camera has
/// just finished arriving somewhere, and where it is standing *is* the thing
/// being recorded rather than a readout that will be stale in a moment.
pub fn query_position<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<Position, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let pan_tilt = camera.pan_tilt_position()?;
    Ok(Position {
        pan: pan_tilt.pan,
        tilt: pan_tilt.tilt,
        zoom: camera.zoom_position()?.value(),
        focus: camera.focus_position()?.value(),
    })
}

/// Formats where the camera is pointing, zoomed and focused.
///
/// Split out from [`format_state`] for front ends that show the camera's power
/// and focus mode in controls of their own: repeating them in a readout
/// underneath only invites the two to disagree.
pub fn format_position(position: &Position) -> String {
    format!(
        "pan={} tilt={} zoom=0x{:04X} focus=0x{:04X}",
        position.pan, position.tilt, position.zoom, position.focus,
    )
}

/// Formats a camera state snapshot in full, for a display with nowhere else to
/// put the power and focus mode.
pub fn format_state(state: &CameraState) -> String {
    let power = if state.power_on { "on" } else { "off" };
    match &state.lens {
        Some(lens) => format!(
            "power={power} {} ({})",
            format_position(&lens.position),
            if lens.auto_focus { "auto" } else { "manual" },
        ),
        // Nothing to say about the lens is itself worth saying: the numbers
        // that were there a moment ago are not the camera's current answer.
        None => format!("power={power} (lens not reporting)"),
    }
}

/// Scripted camera replies for a known position, shared by every module whose
/// tests drive a camera that has to answer for where it is.
#[cfg(test)]
pub(crate) mod fixtures {
    use grafton_visca::testing::testkit::{Step, helpers};

    use super::Position;

    /// A `y0 50 … FF` inquiry reply carrying each value as VISCA's four
    /// nibbles, most significant first.
    fn inquiry_reply(values: &[u16]) -> Vec<u8> {
        let mut reply = vec![0x90, 0x50];
        for value in values {
            reply.extend(
                (0..4)
                    .rev()
                    .map(|nibble| (value >> (nibble * 4)) as u8 & 0x0F),
            );
        }
        reply.push(0xFF);
        reply
    }

    fn answers(inquiry: [u8; 2], values: &[u16]) -> Step {
        helpers::inquiry_response(
            vec![0x81, 0x09, inquiry[0], inquiry[1], 0xFF],
            1,
            inquiry_reply(values),
        )
    }

    /// What a camera standing at `position` replies to the three inquiries
    /// [`super::query_position`] makes, in the order it makes them.
    pub(crate) fn reports(position: Position) -> Vec<Step> {
        vec![
            answers([0x06, 0x12], &[position.pan as u16, position.tilt as u16]),
            answers([0x04, 0x47], &[position.zoom]),
            answers([0x04, 0x48], &[position.focus]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{
        ScriptedBlockingTransport, Step,
        helpers::{self, not_executable},
    };

    fn scripted_camera(
        replies: Vec<Step>,
    ) -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(ScriptedBlockingTransport::new(replies))
            .expect("camera should build from a scripted transport")
    }

    /// The reply a camera gives to something it won't do in its current state.
    fn refusal() -> Step {
        Step::OnSend {
            matches: None,
            responses: vec![not_executable(1)],
        }
    }

    #[test]
    fn a_sleeping_camera_reports_its_power_and_is_not_asked_anything_else() {
        // One scripted reply, so a lens inquiry would find nothing to answer
        // it: the camera is in standby and there is nothing there to ask.
        let camera = scripted_camera(vec![helpers::power_inquiry_response(false)]);

        let state = query_state(&camera).expect("standby is an answer, not a failure");

        assert!(!state.power_on);
        assert!(state.lens.is_none());
    }

    #[test]
    fn a_camera_that_will_not_answer_for_its_lens_yet_still_reports_its_power() {
        // What waking up looks like: power answers, the lens refuses. The
        // power reading is the one that decides what the switch offers, so it
        // has to survive the lens failing beside it.
        let camera = scripted_camera(vec![helpers::power_inquiry_response(true), refusal()]);

        let state = query_state(&camera).expect("a refused lens inquiry is not a failed snapshot");

        assert!(state.power_on);
        assert!(state.lens.is_none());
    }

    #[test]
    fn a_camera_that_answers_nothing_at_all_is_a_failed_snapshot() {
        // The distinction the front ends need: a lens that won't answer is
        // reported, but a camera that won't answer for its own power is not
        // something to make up a state for.
        let camera = scripted_camera(vec![refusal()]);

        assert!(query_state(&camera).is_err());
    }

    fn sample_position() -> Position {
        Position {
            pan: -120,
            tilt: 45,
            zoom: 0x1000,
            focus: 0x2000,
        }
    }

    fn sample_lens() -> Lens {
        Lens {
            position: sample_position(),
            auto_focus: false,
        }
    }

    fn sample_state() -> CameraState {
        CameraState {
            power_on: true,
            lens: Some(sample_lens()),
        }
    }

    #[test]
    fn format_state_renders_all_fields() {
        let text = format_state(&sample_state());
        assert!(text.contains("power=on"));
        assert!(text.contains("pan=-120"));
        assert!(text.contains("tilt=45"));
        assert!(text.contains("zoom=0x1000"));
        assert!(text.contains("focus=0x2000"));
        assert!(text.contains("manual"));
    }

    #[test]
    fn format_state_says_which_way_focus_is_being_driven() {
        let auto = CameraState {
            lens: Some(Lens {
                auto_focus: true,
                ..sample_lens()
            }),
            ..sample_state()
        };

        assert!(format_state(&auto).contains("auto"));
    }

    #[test]
    fn format_state_reports_a_sleeping_camera_without_inventing_a_position() {
        let asleep = CameraState {
            power_on: false,
            lens: None,
        };
        let text = format_state(&asleep);

        assert!(text.contains("power=off"));
        assert!(!text.contains("pan="));
    }

    #[test]
    fn format_position_leaves_out_what_a_control_already_shows() {
        let text = format_position(&sample_position());

        assert!(text.contains("pan=-120"));
        assert!(text.contains("zoom=0x1000"));
        assert!(!text.contains("power"));
        assert!(!text.contains("manual"));
    }

    #[test]
    fn a_position_keeps_each_axis_the_camera_reported_it_for() {
        // Every axis a different value, so a pair swapped anywhere along the
        // way shows up as a wrong number rather than passing by coincidence.
        let camera = scripted_camera(fixtures::reports(sample_position()));

        let position = query_position(&camera).expect("the camera answered every inquiry");

        assert_eq!(position, sample_position());
    }

    #[test]
    fn a_position_the_camera_will_not_finish_reporting_is_not_guessed_at() {
        let mut replies = fixtures::reports(sample_position());
        replies.truncate(1);
        replies.push(refusal());

        assert!(query_position(&scripted_camera(replies)).is_err());
    }
}
