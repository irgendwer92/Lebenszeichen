//! Invitation codes as QR codes.
//!
//! `LZ1-` with Base32 in capitals fits the alphanumeric mode of QR codes
//! (see `docs/drahtformat.md`); an invitation becomes version 8 to 12.

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

#[cfg(test)]
mod tests {
    use super::{QUIET_ZONE, modules, terminal};

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
