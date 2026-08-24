//! The camera's on-screen title: up to twenty characters burned into the
//! video output, which is how an operator labels a shot for whoever is
//! watching downstream rather than for themselves.
//!
//! Hand-written commands, unlike everything else here: grafton-visca has no
//! title support, so these encode `CAM_Title` themselves. The bytes come from
//! the EVI-D70 command list — `8x 01 04 73 pp ...` sets the title in three
//! parts (where it goes, its first ten characters, its last ten) and
//! `8x 01 04 74 pp FF` shows, hides or clears it.

use std::fmt;
use std::time::Duration;

use grafton_visca::{
    BlockingClient, CameraId, Error,
    camera::profiles::GenericVisca,
    command::{CommandBehavior, InquiryResponseSpec, ViscaCommand},
    mode::BlockingFutureExt,
    timeout::{CommandCategory, Deadline},
    transport::{BlockingTransport, HasTransportConfig},
};

/// How many characters a title holds. Shorter ones are padded out with
/// spaces, since the camera has no idea of a title's length — it redraws all
/// twenty every time.
pub const LENGTH: usize = 20;

/// The camera's character set, in code order: a character's code is where it
/// sits in this table.
///
/// Uppercase only, with European accents and currency where a computer would
/// have put lowercase — a camera title says AUDITORIUM, never Auditorium. The
/// Deutsche Mark sign at 0x44 has no character of its own to stand for, so
/// nothing maps to it.
const CHARACTERS: [char; 0x50] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', //
    'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', //
    'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', //
    'Y', 'Z', '&', ' ', '?', '!', '1', '2', //
    '3', '4', '5', '6', '7', '8', '9', '0', //
    'À', 'È', 'Ì', 'Ò', 'Ù', 'Á', 'É', 'Í', //
    'Ó', 'Ú', 'Â', 'Ê', 'Ô', 'Æ', 'Œ', 'Ã', //
    'Õ', 'Ñ', 'Ç', 'ß', 'Ä', 'Ï', 'Ö', 'Ü', //
    'Å', '$', '₣', '¥', '\u{fffd}', '£', '¿', '¡', //
    'ø', '"', ':', '\'', '.', ',', '/', '-',
];

/// The code for a space, which is what a title is padded with and what a
/// character the camera can't draw becomes.
const SPACE: u8 = 0x1B;

/// Where the title sits and how it's drawn: bottom left, white, steady.
///
/// The camera will put it anywhere in an 11x24 grid, in six colours, blinking
/// or not. A caption belongs out of the way at the bottom, and nothing here
/// has yet needed the rest.
const APPEARANCE: [u8; 4] = [0x0A, 0x00, 0x00, 0x00];

/// A title as the camera holds it: twenty characters in the camera's own
/// character set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Title([u8; LENGTH]);

impl Title {
    /// Encodes `text` for the camera, keeping the first [`LENGTH`] characters
    /// it can draw.
    ///
    /// Anything outside the camera's character set becomes a space rather
    /// than an error: the point of the title is to be legible on a monitor,
    /// and a missing accent is a better outcome than a refused caption.
    pub fn new(text: &str) -> Self {
        let mut characters = [SPACE; LENGTH];
        for (slot, character) in characters.iter_mut().zip(text.chars()) {
            *slot = encode(character);
        }
        Self(characters)
    }

    /// The title as the camera will draw it, without its trailing padding.
    pub fn text(&self) -> String {
        decode(&self.0).trim_end().to_string()
    }
}

impl fmt::Display for Title {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.text())
    }
}

/// Every character the camera can draw, for a front end sizing a box that has
/// to hold [`LENGTH`] of them.
///
/// The widest of these is as wide as a title can get: anything else typed into
/// a title becomes a space on the way out, so a box that fits twenty of the
/// widest fits every title the camera will ever draw.
pub fn drawable() -> impl Iterator<Item = char> {
    CHARACTERS
        .iter()
        .copied()
        .filter(|character| *character != char::REPLACEMENT_CHARACTER)
}

/// The camera's code for `character`, or a space if it can't draw it.
fn encode(character: char) -> u8 {
    let code = |character| {
        CHARACTERS
            .iter()
            .position(|drawn| *drawn == character)
            .map(|code| code as u8)
    };
    // Uppercased second, not first: 'ß' uppercases to "SS", and the camera
    // has a ß of its own that says it better.
    code(character)
        .or_else(|| character.to_uppercase().find_map(code))
        .unwrap_or(SPACE)
}

/// What the camera draws for `codes` — the inverse of [`encode`], for showing
/// a title back to whoever typed it.
pub fn decode(codes: &[u8]) -> String {
    codes
        .iter()
        .map(|code| {
            CHARACTERS
                .get(usize::from(*code))
                .copied()
                .unwrap_or('\u{fffd}')
        })
        .collect()
}

/// One `CAM_Title` message: where the title goes, or ten of its characters.
struct TitleSet {
    /// 0x00 for the appearance, 0x01 for the first ten characters, 0x02 for
    /// the last ten.
    part: u8,
    payload: [u8; 10],
}

impl ViscaCommand for TitleSet {
    const MAX_SIZE: usize = 16;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        buffer[..5].copy_from_slice(&[camera_id.to_address_byte(), 0x01, 0x04, 0x73, self.part]);
        buffer[5..15].copy_from_slice(&self.payload);
        buffer[15] = 0xFF;
        Ok(Self::MAX_SIZE)
    }
}

/// The `CAM_Title` message that shows, hides or clears whatever title the
/// camera is holding.
struct TitleDisplay(u8);

impl ViscaCommand for TitleDisplay {
    const MAX_SIZE: usize = 6;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        buffer[..6].copy_from_slice(&[camera_id.to_address_byte(), 0x01, 0x04, 0x74, self.0, 0xFF]);
        Ok(Self::MAX_SIZE)
    }
}

/// How long to wait for the camera to say whether it has titles.
///
/// Deliberately short: this is asked as part of connecting, with the operator
/// watching, and the camera has just answered a version inquiry over the same
/// link — one that has nothing to say about titles within two seconds isn't
/// going to say it in sixty. Running out of time here is not an answer either
/// way, and costs nothing but a title command sent to a camera that may refuse
/// it once.
const ANSWER_TIME: Duration = Duration::from_secs(2);

/// `CAM_TitleDisplayModeInq`, which asks whether the title is currently
/// showing — and, in doing so, whether there is such a thing as a title here
/// at all.
///
/// What it replies with is of no interest: the front end doesn't show the
/// camera's title, it sends its own. It is the inquiry rather than one of the
/// commands because an inquiry changes nothing — a camera that does have the
/// feature comes through this with whatever title it was holding intact,
/// where clearing one to find out would have thrown it away.
struct TitleDisplayInquiry;

impl ViscaCommand for TitleDisplayInquiry {
    const MAX_SIZE: usize = 5;

    /// The library's shortest category, though [`ANSWER_TIME`] cuts this
    /// shorter still: nothing about this inquiry is slow, and the sixty
    /// seconds a hand-written command is given by default are for commands
    /// that move something.
    const TIMEOUT_CATEGORY: CommandCategory = CommandCategory::Quick;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        buffer[..5].copy_from_slice(&[camera_id.to_address_byte(), 0x09, 0x04, 0x74, 0xFF]);
        Ok(Self::MAX_SIZE)
    }

    fn behavior(&self) -> CommandBehavior {
        CommandBehavior::Inquiry(InquiryResponseSpec::Raw)
    }
}

/// Whether this camera has titles at all, asked of the camera itself.
///
/// `CAM_Title` belongs to the EVI-D70 generation; the cameras that followed
/// dropped the feature, and an EVI-D80 answers a syntax error to every part of
/// it. A syntax error is the whole of the evidence — it is the protocol's way
/// of saying it doesn't know the command, and the only refusal that means the
/// feature is absent rather than momentarily unavailable. Anything else, a
/// camera that is asleep or busy or silent included, counts as having titles,
/// because none of those are the camera saying it hasn't.
pub fn supported<T>(camera: &BlockingClient<GenericVisca, T>) -> bool
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let answer = camera
        .send_command_with_deadline(&TitleDisplayInquiry, Deadline::from_timeout(ANSWER_TIME))
        .block();
    !matches!(answer, Err(Error::SyntaxError))
}

/// Gives the camera a title to hold, without showing it.
///
/// Three messages, because that's how the camera takes one: its appearance,
/// then its characters ten at a time.
pub fn set_title<T>(camera: &BlockingClient<GenericVisca, T>, title: Title) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let mut appearance = [0x00; 10];
    appearance[..4].copy_from_slice(&APPEARANCE);

    let (first, second) = title.0.split_at(LENGTH / 2);
    let half = |characters: &[u8]| {
        <[u8; 10]>::try_from(characters).expect("a title splits into two halves of ten")
    };
    for (part, payload) in [
        (0x00, appearance),
        (0x01, half(first)),
        (0x02, half(second)),
    ] {
        camera.execute(TitleSet { part, payload })?;
    }
    Ok(())
}

/// Shows the title the camera is holding, or hides it again.
pub fn show_title<T>(camera: &BlockingClient<GenericVisca, T>, on: bool) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.execute(TitleDisplay(if on { 0x02 } else { 0x03 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, Step, helpers};

    fn scripted_camera(
        acks: usize,
    ) -> (
        BlockingClient<GenericVisca, ScriptedBlockingTransport>,
        ScriptedBlockingTransport,
    ) {
        let transport = ScriptedBlockingTransport::new(
            (0..acks)
                .map(|_| helpers::standard_command_response(1))
                .collect::<Vec<_>>(),
        );
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport.clone())
            .expect("camera should build from a scripted transport");
        (camera, transport)
    }

    #[test]
    fn a_title_is_encoded_in_the_cameras_own_character_set() {
        let title = Title::new("HYMN 123");

        assert_eq!(
            &title.0[..8],
            // H Y M N _ 1 2 3
            &[0x07, 0x18, 0x0C, 0x0D, SPACE, 0x1E, 0x1F, 0x20]
        );
    }

    #[test]
    fn a_title_is_padded_out_to_the_length_the_camera_redraws() {
        let title = Title::new("A");

        assert_eq!(title.0.len(), LENGTH);
        assert!(title.0[1..].iter().all(|code| *code == SPACE));
        assert_eq!(title.text(), "A");
    }

    #[test]
    fn a_title_longer_than_the_camera_can_show_is_cut_to_fit() {
        let title = Title::new("THE WHOLE CONGREGATION STANDING");

        assert_eq!(title.text(), "THE WHOLE CONGREGATI");
    }

    #[test]
    fn lowercase_is_shown_in_the_uppercase_the_camera_has() {
        assert_eq!(Title::new("Chorister").text(), "CHORISTER");
    }

    #[test]
    fn a_character_the_camera_cannot_draw_becomes_a_space() {
        assert_eq!(Title::new("A\u{2603}B").text(), "A B");
    }

    #[test]
    fn set_title_sends_the_appearance_then_the_characters_in_two_halves() {
        let (camera, transport) = scripted_camera(3);

        set_title(&camera, Title::new("PODIUM")).expect("scripted camera should ack each part");

        let sent = transport.sent();
        assert_eq!(sent.len(), 3);
        assert_eq!(
            sent[0],
            vec![
                0x81, 0x01, 0x04, 0x73, 0x00, 0x0A, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0xFF
            ]
        );
        assert_eq!(
            sent[1],
            vec![
                0x81, 0x01, 0x04, 0x73, 0x01, // P O D I U M, then padding
                0x0F, 0x0E, 0x03, 0x08, 0x14, 0x0C, SPACE, SPACE, SPACE, SPACE, 0xFF
            ]
        );
        assert_eq!(&sent[2][..5], &[0x81, 0x01, 0x04, 0x73, 0x02]);
        assert!(sent[2][5..15].iter().all(|code| *code == SPACE));
    }

    #[test]
    fn show_title_turns_the_display_on_and_off() {
        let (camera, transport) = scripted_camera(2);

        show_title(&camera, true).expect("scripted camera should ack the display on");
        show_title(&camera, false).expect("scripted camera should ack the display off");

        assert_eq!(
            transport.sent(),
            vec![
                vec![0x81, 0x01, 0x04, 0x74, 0x02, 0xFF],
                vec![0x81, 0x01, 0x04, 0x74, 0x03, 0xFF],
            ]
        );
    }

    #[test]
    fn a_camera_that_answers_the_title_inquiry_has_titles() {
        let transport = ScriptedBlockingTransport::new(vec![helpers::inquiry_response(
            vec![0x81, 0x09, 0x04, 0x74, 0xFF],
            1,
            // "showing", which is as much as this camera has to say about it.
            vec![0x90, 0x50, 0x02, 0xFF],
        )]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport.clone())
            .expect("camera should build from a scripted transport");

        assert!(supported(&camera));
        assert_eq!(transport.sent(), vec![vec![0x81, 0x09, 0x04, 0x74, 0xFF]]);
    }

    #[test]
    fn a_camera_that_calls_the_title_inquiry_a_syntax_error_has_no_titles() {
        let transport = ScriptedBlockingTransport::new(vec![helpers::errors::syntax_error(1)]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        assert!(!supported(&camera));
    }

    #[test]
    fn a_camera_that_cannot_answer_the_title_inquiry_yet_keeps_its_titles() {
        // "Not executable" is what a camera in standby says to almost
        // anything, and it is not the camera saying it has no titles — nor is
        // silence, or a link that dropped a byte. Only a syntax error means
        // the command isn't there.
        let transport = ScriptedBlockingTransport::new(vec![Step::OnSend {
            matches: None,
            responses: vec![helpers::not_executable(1)],
        }]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        assert!(supported(&camera));
    }

    #[test]
    fn the_characters_offered_for_measuring_are_the_ones_the_camera_draws() {
        for character in drawable() {
            assert_eq!(
                decode(&[encode(character)]),
                character.to_string(),
                "{character} is offered as one the camera draws"
            );
        }

        // Every code but the Deutsche Mark sign, which stands for nothing that
        // could be typed and so is nothing to measure.
        assert_eq!(drawable().count(), CHARACTERS.len() - 1);
    }

    #[test]
    fn decode_shows_a_code_the_camera_has_no_character_for() {
        assert_eq!(decode(&[0x00, 0x7F]), "A\u{fffd}");
    }
}
