//! Codes of Lebenszeichen as QR codes, and reading them back from a camera.
//!
//! Invitations, device codes and setup codes are all of this kind. `LZ1-` with Base32 in capitals fits the alphanumeric mode of QR codes
//! (see `docs/wire-format.md`); an invitation becomes version 8 to 12.

use lz_core::wire::CodeKind;
use qrcode::render::unicode::Dense1x2;
use qrcode::{Color, EcLevel, QrCode};
use serde::Serialize;

/// Modules of light margin around the code, as the standard asks for.
pub const QUIET_ZONE: usize = 4;

/// The modules of a QR code, without the quiet zone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Qr {
    /// Modules per side.
    pub width: usize,
    /// Row by row, `true` for dark.
    pub dark: Vec<bool>,
}

fn encode(text: &str) -> QrCode {
    // Level M survives a bit of glare on a screen. Every invitation fits:
    // at most 419 characters, and version 40 takes far more.
    QrCode::with_error_correction_level(text, EcLevel::M)
        .expect("an invitation always fits into a QR code")
}

/// Encodes a text, for drawing it in a window.
pub fn modules(text: &str) -> Qr {
    let code = encode(text);
    Qr {
        width: code.width(),
        dark: code
            .to_colors()
            .into_iter()
            .map(|color| color == Color::Dark)
            .collect(),
    }
}

/// Encodes a text for a terminal, two rows of modules per line.
///
/// Like `qrencode -t UTF8`, this prints the light modules as blocks, so the
/// code reads right on a dark terminal with light text.
pub fn terminal(text: &str) -> String {
    encode(text)
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build()
}

/// Finds a code of Lebenszeichen (invitation, device code or setup code) in
/// a greyscale image, such as the brightness plane of a camera frame:
/// `width` × `height` pixels, one byte each, rows `row_stride` bytes apart.
/// Other QR codes in the picture are ignored.
///
/// Returns `None` if the buffer is too small for the given size.
pub fn decode_code(luma: &[u8], width: usize, height: usize, row_stride: usize) -> Option<String> {
    if width == 0 || height == 0 || row_stride < width {
        return None;
    }
    let needed = (height - 1).checked_mul(row_stride)?.checked_add(width)?;
    if luma.len() < needed {
        return None;
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * row_stride + x]);
    image
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .find(|text| CodeKind::of(text).is_some())
}

#[cfg(test)]
mod tests {
    use super::{QUIET_ZONE, decode_code, modules, terminal};

    /// An invitation of typical length: no voucher, short relay address.
    fn invitation() -> String {
        format!("LZ1-{}", "A".repeat(220))
    }

    /// Reads a code back, with the quiet zone a scanner needs.
    fn decode(width: usize, dark: impl Fn(usize, usize) -> bool) -> String {
        let side = width + 2 * QUIET_ZONE;
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(side * 4, side * 4, |x, y| {
            let (x, y) = (x / 4, y / 4);
            let inside = (QUIET_ZONE..QUIET_ZONE + width).contains(&x)
                && (QUIET_ZONE..QUIET_ZONE + width).contains(&y);
            if inside && dark(x - QUIET_ZONE, y - QUIET_ZONE) {
                0
            } else {
                255
            }
        });
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1);
        grids[0].decode().expect("readable").1
    }

    #[test]
    fn modules_read_back_to_the_invitation() {
        let text = invitation();
        let qr = modules(&text);
        assert_eq!(qr.dark.len(), qr.width * qr.width);
        // Alphanumeric mode keeps it small: version 9 at level M.
        assert_eq!(qr.width, 53);
        assert_eq!(decode(qr.width, |x, y| qr.dark[y * qr.width + x]), text);
    }

    #[test]
    fn longest_invitation_still_fits() {
        // Voucher and a relay address of 120 characters: 419 characters.
        let text = format!("LZ1-{}", "A".repeat(415));
        let qr = modules(&text);
        assert_eq!(qr.width, 65); // version 12
        assert_eq!(decode(qr.width, |x, y| qr.dark[y * qr.width + x]), text);
    }

    /// Draws a code into a camera-like frame: `scale` pixels per module,
    /// at an offset, optionally turned by 90°, with padding at row ends.
    fn frame(text: &str, scale: usize, turned: bool) -> (Vec<u8>, usize, usize, usize) {
        let qr = modules(text);
        let (width, height, stride) = (640, 480, 656);
        let mut luma = vec![200u8; stride * height];
        let (left, top) = (90, 40);
        let side = qr.width * scale;
        for y in 0..side {
            for x in 0..side {
                let (mx, my) = (x / scale, y / scale);
                let (mx, my) = if turned {
                    (my, qr.width - 1 - mx)
                } else {
                    (mx, my)
                };
                if qr.dark[my * qr.width + mx] {
                    luma[(top + y) * stride + left + x] = 30;
                }
            }
        }
        (luma, width, height, stride)
    }

    #[test]
    fn invitation_is_found_in_a_camera_frame() {
        let text = invitation();
        for turned in [false, true] {
            let (luma, width, height, stride) = frame(&text, 7, turned);
            assert_eq!(
                decode_code(&luma, width, height, stride).as_deref(),
                Some(text.as_str()),
                "turned: {turned}"
            );
        }
    }

    #[test]
    fn device_and_setup_codes_are_found_too() {
        for text in [
            format!("LZD1-{}", "A".repeat(120)),
            format!("LZS1-{}", "B".repeat(200)),
        ] {
            let (luma, width, height, stride) = frame(&text, 7, false);
            assert_eq!(
                decode_code(&luma, width, height, stride).as_deref(),
                Some(text.as_str())
            );
        }
    }

    #[test]
    fn other_codes_and_empty_frames_give_nothing() {
        let (luma, width, height, stride) = frame("https://example.org", 7, false);
        assert_eq!(decode_code(&luma, width, height, stride), None);
        let blank = vec![200u8; 640 * 480];
        assert_eq!(decode_code(&blank, 640, 480, 640), None);
    }

    #[test]
    fn too_small_buffers_are_refused() {
        assert_eq!(decode_code(&[0; 10], 640, 480, 640), None);
        assert_eq!(decode_code(&[0; 10], 0, 0, 0), None);
        assert_eq!(decode_code(&[0; 100], 10, 10, 5), None);
    }

    #[test]
    fn terminal_shows_light_modules_as_blocks() {
        let text = invitation();
        let qr = modules(&text);
        let rendered = terminal(&text);
        let lines: Vec<&str> = rendered.lines().collect();
        let side = qr.width + 2 * QUIET_ZONE;
        assert_eq!(lines.len(), side.div_ceil(2));
        assert!(lines.iter().all(|line| line.chars().count() == side));
        // The quiet zone is light, so the first line is all blocks.
        assert!(lines[0].chars().all(|c| c == '█'));
    }
}
