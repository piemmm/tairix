//! PDF: one page of a document read into a drawing through `lib/pdf`, and a
//! drawing written as a one-page document (`plans/PDF.md`, `plans/DRAW.md`).

mod read;
mod write;

pub use read::{open, read, recognised, Opened, Request, MAX_PASSWORD_BYTES};
pub use tairix_pdf::Refused;
pub use write::{pdf, Pdf};
