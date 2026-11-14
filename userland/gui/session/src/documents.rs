//! An application's own documents folder: the one folder beneath the user's
//! `Home/Documents/` its signed manifest names, granted it writable without a
//! picker (`plans/BEEB.md` §6).
//!
//! The session makes the folder where it is missing and marks what it makes
//! with the application's identity, its publisher and bundle id, in the
//! [`OWNER_MARK`] attribute, which an application without filesystem
//! authority cannot set. It grants only a folder whose mark is the
//! requester's, opened without following a link, so a manifest naming a
//! folder the user made, or another application's, reaches nothing. The path
//! to `Home/Documents/` is the user's own and is followed as they laid it out.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::home::{HOME_FILES_DIR, HOME_FILES_DOCUMENTS_DIR};
use tairix_abi::window_ipc::DocumentsRefusal;
use tairix_abi::{AppIdentity, Errno, ProcId};
use tairix_log::{EventId, Field, FieldValue, Sink};

/// The attribute a folder the session made for an application carries: the
/// application's publisher, then its bundle id.
pub const OWNER_MARK: &[u8] = b"tairix.documents-owner";

/// The folders under the user's home every application's documents folder is
/// in, outermost first.
pub const DOCUMENTS: [&str; 2] = [HOME_FILES_DIR, HOME_FILES_DOCUMENTS_DIR];

/// Event id of an application granted its documents folder.
pub const DOCUMENTS_FOLDER_GRANTED: EventId = EventId(20_044);

/// The exact message [`DOCUMENTS_FOLDER_GRANTED`] is emitted with.
pub const DOCUMENTS_FOLDER_GRANTED_MESSAGE: &str = "documents folder granted";

/// Event id of an application refused its documents folder, with why.
pub const DOCUMENTS_FOLDER_REFUSED: EventId = EventId(20_045);

/// The exact message [`DOCUMENTS_FOLDER_REFUSED`] is emitted with.
pub const DOCUMENTS_FOLDER_REFUSED_MESSAGE: &str = "documents folder refused";

/// Why a request was refused before any folder was looked at.
pub mod before {
    /// The caller runs no verified bundle.
    pub const NO_APPLICATION: &str = "no verified application";
    /// Its installed manifest names no documents folder.
    pub const NOT_DECLARED: &str = "its manifest names no documents folder";
    /// The name its manifest gives is not one visible folder name.
    pub const NOT_A_NAME: &str = "its manifest names no visible folder";
    /// One is already owed to it.
    pub const OWED: &str = "one is already owed";
    /// The desktop holds as many filesystem calls as it may.
    pub const BUSY: &str = "the desktop is waiting on too many files";
    /// The folder could not be delegated.
    pub const UNDELEGATED: &str = "the folder could not be delegated";
}

/// Why a [`DocumentsRefusal`] was given, for the record.
#[must_use]
pub const fn reason(refusal: DocumentsRefusal) -> &'static str {
    match refusal {
        DocumentsRefusal::NotTheirs => "the folder there is not its own",
        DocumentsRefusal::NotAFolder => "something other than a folder is there",
        DocumentsRefusal::Unavailable => "the user's documents are out of reach",
    }
}

/// Record what became of `app`'s request: granted, or refused for `reason`.
/// The folder's name is the application's declaration and the user's file,
/// and stays out of the record.
pub fn note<S: Sink + ?Sized>(audit: &S, app: Option<&AppIdentity>, outcome: Result<(), &str>) {
    let caller = Field {
        key: "caller",
        value: FieldValue::Str(app.map_or("", AppIdentity::bundle_id)),
    };
    let event = |level, id, message, fields: &[Field<'_>]| {
        tairix_log::log(
            audit,
            &tairix_log::Event {
                level,
                id,
                message,
                fields,
            },
        );
    };
    match outcome {
        Ok(()) => event(
            tairix_log::Level::Info,
            DOCUMENTS_FOLDER_GRANTED,
            DOCUMENTS_FOLDER_GRANTED_MESSAGE,
            &[caller],
        ),
        Err(why) => event(
            tairix_log::Level::Warn,
            DOCUMENTS_FOLDER_REFUSED,
            DOCUMENTS_FOLDER_REFUSED_MESSAGE,
            &[
                caller,
                Field {
                    key: "reason",
                    value: FieldValue::Str(why),
                },
            ],
        ),
    }
}

/// A documents-folder request the window engine accepted, waiting for the
/// file worker: the window it is answered to and the process that owns it,
/// the application asking, and the folder its manifest names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentsAsk {
    /// The window the answer is addressed to.
    pub window_id: u64,
    /// The process that asked, which owns the window.
    pub owner: ProcId,
    /// The kernel-attested application.
    pub app: AppIdentity,
    /// The folder its installed manifest names, already a visible name.
    pub name: String,
}

/// The window host's side of the documents folder: which folder an
/// application's installed manifest names, and where an accepted request goes.
pub trait DocumentsFolders {
    /// The folder `app`'s installed, signed manifest names, if it names one.
    fn named(&self, app: &AppIdentity) -> Option<String>;

    /// Hand `ask` on to be carried out off the loop.
    fn ask(&mut self, ask: DocumentsAsk);
}

/// A host that serves no request: it knows no manifest and takes no request.
pub struct NoDocuments;

impl DocumentsFolders for NoDocuments {
    fn named(&self, _app: &AppIdentity) -> Option<String> {
        None
    }

    fn ask(&mut self, _ask: DocumentsAsk) {}
}

/// The filesystem the folder is found or made in, as the session reaches it.
pub trait DocumentsFs {
    /// The user's documents folder.
    type Documents;
    /// A folder beneath it, open to be read and granted.
    type Folder;

    /// The user's `Home/Documents/`, made where it is missing.
    ///
    /// # Errors
    ///
    /// Whatever keeps it from being reached or made.
    fn documents(&mut self) -> Result<Self::Documents, Errno>;

    /// The folder `name` beneath `documents`, a link at `name` never
    /// followed.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for none, [`Errno::LinkLoop`] or
    /// [`Errno::NotADirectory`] for something else there, and any other
    /// refusal.
    fn open(&mut self, documents: &Self::Documents, name: &str) -> Result<Self::Folder, Errno>;

    /// Make the folder `name` beneath `documents` and mark the folder made
    /// `mark`, taking it away again if it cannot be marked.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] when something took the name first, and any
    /// refusal to make or mark it.
    fn make(&mut self, documents: &Self::Documents, name: &str, mark: &[u8]) -> Result<(), Errno>;

    /// The mark `folder` carries, if it carries one.
    ///
    /// # Errors
    ///
    /// Any refusal to read it.
    fn mark(&mut self, folder: &Self::Folder) -> Result<Option<Vec<u8>>, Errno>;
}

/// The mark naming `app` as a folder's owner.
#[must_use]
pub fn owner_mark(app: &AppIdentity) -> Vec<u8> {
    let mut mark = Vec::with_capacity(app.publisher().as_bytes().len() + app.bundle_id().len());
    mark.extend_from_slice(app.publisher().as_bytes());
    mark.extend_from_slice(app.bundle_id().as_bytes());
    mark
}

/// `app`'s documents folder `name`, made and marked where it is missing.
///
/// # Errors
///
/// The [`DocumentsRefusal`] the request is answered with.
pub fn open_documents_folder<F: DocumentsFs>(
    fs: &mut F,
    name: &str,
    app: &AppIdentity,
) -> Result<F::Folder, DocumentsRefusal> {
    let mark = owner_mark(app);
    let documents = fs.documents().map_err(|_| DocumentsRefusal::Unavailable)?;
    let folder = match fs.open(&documents, name) {
        Ok(folder) => folder,
        Err(Errno::NotFound) => {
            match fs.make(&documents, name, &mark) {
                // Made by another first: its mark decides.
                Ok(()) | Err(Errno::AlreadyExists) => {}
                Err(_) => return Err(DocumentsRefusal::Unavailable),
            }
            fs.open(&documents, name).map_err(refusal_of)?
        }
        Err(err) => return Err(refusal_of(err)),
    };
    match fs.mark(&folder) {
        Ok(Some(found)) if found == mark => Ok(folder),
        Ok(_) => Err(DocumentsRefusal::NotTheirs),
        Err(_) => Err(DocumentsRefusal::Unavailable),
    }
}

/// The refusal an open of the folder's name is answered with.
const fn refusal_of(err: Errno) -> DocumentsRefusal {
    match err {
        Errno::LinkLoop | Errno::NotADirectory => DocumentsRefusal::NotAFolder,
        _ => DocumentsRefusal::Unavailable,
    }
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;
