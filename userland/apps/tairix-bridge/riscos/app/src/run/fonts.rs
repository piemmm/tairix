//! The font worker: a session's glyph requests carried to the font service,
//! and the desktop font a session is told of, worked out from the desktop's
//! text and its family's line box. Each is a round trip to the font
//! service, so neither runs on the window loop.

use alloc::vec::Vec;

use tairix_abi::desktop::DesktopText;
use tairix_abi::font_ipc::FontRequest;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_rt::work::Worker;
use tairix_util::defer::JobQueue;

use crate::front::SessionId;

/// The font worker.
pub type FontWorker = Worker<(), Job, Done, JobQueue<Job, Done>>;

/// The jobs waiting at once: a glyph request from each of a few sessions,
/// beside the desktop's text.
pub const JOBS: usize = 16;

/// A job.
pub enum Job {
    /// A glyph request of `session`'s.
    Relay {
        /// Whose.
        session: SessionId,
        /// Its serial.
        serial: u32,
        /// The request.
        request: Vec<u8>,
    },
    /// The desktop font of the desktop's text, none where each theme draws
    /// its own.
    Desktop(Option<DesktopText>),
}

/// What a job came to.
pub enum Done {
    /// The reply to `session`'s relay `serial`, none where it could not be
    /// carried.
    Relayed {
        /// Whose.
        session: SessionId,
        /// Its serial.
        serial: u32,
        /// The reply.
        reply: Option<Vec<u8>>,
    },
    /// The desktop font.
    Desktop(DesktopFont),
}

/// One job done.
pub fn serve((): &mut (), job: &mut Job) -> Done {
    match job {
        Job::Relay {
            session,
            serial,
            request,
        } => Done::Relayed {
            session: *session,
            serial: *serial,
            reply: relayed(request),
        },
        Job::Desktop(text) => Done::Desktop(desktop_font(*text)),
    }
}

/// The font service's reply to `request`, held to the one request the Font
/// Manager makes.
fn relayed(request: &[u8]) -> Option<Vec<u8>> {
    let request = FontRequest::from_bytes(request).ok()?;
    let mut reply = Vec::new();
    let len = tairix_font::relay_outlines(&request, &mut reply)?;
    reply.truncate(len);
    Some(reply)
}

/// The desktop font `text` names: its family, at its size in sixteenths of
/// a point through the family's line box, where the font service knows the
/// family.
fn desktop_font(text: Option<DesktopText>) -> DesktopFont {
    let Some(text) = text else {
        return DesktopFont::SHIPPED;
    };
    let size = tairix_font::families()
        .iter()
        .find(|entry| entry.key == text.family())
        .map(|entry| tairix_theme::sixteenths_of(text.size_px(), entry.line_box()));
    DesktopFont {
        family: text.family(),
        size,
    }
}
