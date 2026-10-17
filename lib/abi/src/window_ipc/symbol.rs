//! What a menu row does, named from the one vocabulary every menu shares.
//!
//! An application states a row's *function* and the desktop chooses the
//! picture that shows it, so Delete looks alike in every application and a
//! redrawn picture reaches every menu at once.

use crate::Errno;

/// What a menu row does: the function its picture states.
///
/// Closed, with contiguous wire values from 1, so an unknown value on decode
/// is refused rather than guessed at and zero names no function at all.
/// Several functions may share a picture; a function has exactly one.
#[repr(u16)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AppMenuSymbol {
    /// Open the item, or choose a document to open.
    Open = 1,
    /// Open the item in an application the user picks.
    OpenWith,
    /// Show a folder: the one holding an item, or one to choose.
    OpenFolder,
    /// An application the item could be opened with.
    Application,
    /// Open another window of this application.
    NewWindow,
    /// Start a new document, picture, page or sprite.
    NewDocument,
    /// Make a new folder.
    NewFolder,
    /// Write the document back where it came from.
    Save,
    /// Write the document somewhere the user chooses.
    SaveAs,
    /// Close this window or document.
    Close,
    /// End this application.
    Quit,
    /// What the application, the system or the item is.
    Info,
    /// An item's facts and settings.
    Properties,
    /// The settings of an application or of a part of the system.
    Settings,
    /// The document menu.
    File,
    /// The editing menu.
    Edit,
    /// The menu of ways to show what the window holds.
    View,
    /// Move the selection onto the clipboard.
    Cut,
    /// Copy the selection onto the clipboard.
    Copy,
    /// Put the clipboard's contents here.
    Paste,
    /// Select everything.
    SelectAll,
    /// Select nothing.
    SelectNone,
    /// Give the item a new name.
    Rename,
    /// Make a copy of the item beside it.
    Duplicate,
    /// Destroy the item.
    Delete,
    /// Take the item out of a list, leaving the item itself be.
    Remove,
    /// Empty a screen or a list.
    Clear,
    /// Take back the last change.
    Undo,
    /// Make again the change last taken back.
    Redo,
    /// Move the lines in by one level.
    Indent,
    /// Move the lines out by one level.
    Outdent,
    /// Comment the lines out, or back in.
    Comment,
    /// Typing replaces what is under the caret.
    Overwrite,
    /// Look for text.
    Find,
    /// Look for text and replace it.
    Replace,
    /// Go to the next match.
    FindNext,
    /// Go to the previous match.
    FindPrevious,
    /// Go to a place given by number or name.
    GoTo,
    /// Go to the next problem found in the document.
    NextProblem,
    /// Show the document as text.
    ViewText,
    /// Show the document's bytes as hexadecimal.
    ViewHex,
    /// Show the document's structure, then its bytes.
    ViewData,
    /// Show the document's machine code.
    ViewCode,
    /// The instruction set machine code is read as.
    InstructionSet,
    /// The syntax a document is coloured as.
    Syntax,
    /// How far apart tab stops lie.
    TabWidth,
    /// How lines are ended.
    LineEndings,
    /// Show more closely.
    ZoomIn,
    /// Show from further away.
    ZoomOut,
    /// Show it whole in the window.
    ZoomFit,
    /// Show it as wide as the window.
    FitWidth,
    /// Show it at its own size.
    ZoomActual,
    /// Show it at this magnification.
    ZoomLevel,
    /// Draw the text larger.
    TextLarger,
    /// Draw the text smaller.
    TextSmaller,
    /// Draw the text at its own size.
    TextActual,
    /// Turn it a quarter turn anticlockwise.
    RotateLeft,
    /// Turn it a quarter turn clockwise.
    RotateRight,
    /// Turn it half a turn.
    RotateHalf,
    /// Mirror it left to right.
    FlipHorizontal,
    /// Mirror it top to bottom.
    FlipVertical,
    /// Show a grid over it.
    Grid,
    /// Show the boundaries between pixels.
    PixelGrid,
    /// The panes beside the work.
    Panes,
    /// Put the panes back where they started.
    ResetPanes,
    /// How a window shows its items.
    Display,
    /// Each item as a large picture above its name.
    LargeIcons,
    /// Each item as a small picture beside its name.
    SmallIcons,
    /// Each item as a row of facts.
    List,
    /// Read it again now.
    Refresh,
    /// Order by name.
    SortByName,
    /// Order by kind.
    SortByKind,
    /// Order by size.
    SortBySize,
    /// Order by date.
    SortByDate,
    /// Lay the icons out from the left.
    ArrangeLeft,
    /// Lay the icons out from the right.
    ArrangeRight,
    /// Choose the desktop's picture.
    Wallpaper,
    /// Put a shortcut to it on the desktop.
    Shortcut,
    /// Show it.
    Visible,
    /// Hide it.
    Hidden,
    /// The user's own home folder.
    Home,
    /// A folder.
    Folder,
    /// A storage volume.
    Drive,
    /// Go to the one before.
    Previous,
    /// Go to the one after.
    Next,
    /// Move it up one place.
    MoveUp,
    /// Move it down one place.
    MoveDown,
    /// The picture's layers.
    Layers,
    /// Add a layer.
    LayerNew,
    /// Go to the layer above.
    LayerAbove,
    /// Go to the layer below.
    LayerBelow,
    /// Move the layer up the stack.
    LayerRaise,
    /// Move the layer down the stack.
    LayerLower,
    /// Merge the layer into the one below it.
    MergeDown,
    /// Merge every layer into one.
    Flatten,
    /// The menu of changes to the whole picture.
    Image,
    /// Change the picture's size, scaling what it shows.
    Resize,
    /// Change the picture's size, keeping what it shows.
    CanvasSize,
    /// Swap every colour for its opposite.
    InvertColours,
    /// Change how the picture's colours are held.
    ColourModel,
    /// Give the picture a mask.
    MaskAdd,
    /// Take the picture's mask away.
    MaskRemove,
    /// Fill the selection with the primary colour.
    FillSelection,
    /// Cut the picture down to the selection.
    CropToSelection,
    /// Set down what is being carried.
    PutDown,
    /// Clear what is selected to nothing.
    Erase,
    /// The colours being painted with.
    Colours,
    /// Choose the primary colour.
    ColourPrimary,
    /// Choose the secondary colour.
    ColourSecondary,
    /// Swap the primary and secondary colours.
    ColourSwap,
    /// Put the colours back to black and white.
    ColourReset,
    /// Take a colour from the picture.
    PickColour,
    /// The menu of adjustments and filters.
    Adjust,
    /// Change brightness and contrast.
    Brightness,
    /// Change hue and saturation.
    HueSaturation,
    /// Change the balance of colours.
    ColourBalance,
    /// Change the tonal levels.
    Levels,
    /// Change the tone curve.
    Curves,
    /// Correct the white balance.
    WhiteBalance,
    /// Reduce each colour to fewer levels.
    Posterise,
    /// Reduce every pixel to black or white.
    Threshold,
    /// Take the colour out.
    Desaturate,
    /// Soften it.
    Blur,
    /// Sharpen it.
    Sharpen,
    /// Coarsen it into blocks.
    Pixelate,
    /// Scatter noise over it.
    Noise,
    /// Trace its edges.
    FindEdges,
    /// The picture's sprites or pages.
    Pages,
    /// The tools.
    Tools,
    /// Choose the selection tool.
    ToolSelect,
    /// Choose the pencil.
    ToolPencil,
    /// Choose the brush.
    ToolBrush,
    /// Choose the airbrush.
    ToolAirbrush,
    /// Choose the eraser.
    ToolEraser,
    /// Choose the clone stamp.
    ToolClone,
    /// Choose the fill.
    ToolFill,
    /// Choose the gradient.
    ToolGradient,
    /// Choose the eyedropper.
    ToolEyedropper,
    /// Choose the text tool.
    ToolText,
    /// Choose the line tool.
    ToolLine,
    /// Choose the rectangle tool.
    ToolRectangle,
    /// Choose the ellipse tool.
    ToolEllipse,
    /// Choose the polygon tool.
    ToolPolygon,
    /// Choose the crop tool.
    ToolCrop,
    /// Choose the hand.
    ToolHand,
    /// Choose the zoom tool.
    ToolZoom,
    /// Watch what the machine is doing.
    SystemMonitor,
    /// A shell for managing tasks.
    TaskShell,
    /// Lock the screen.
    LockScreen,
    /// Switch to another user's session.
    SwitchUser,
    /// End this session.
    LogOut,
    /// Restart the machine.
    Restart,
    /// Turn the machine off.
    ShutDown,
    /// The time.
    Clock,
    /// Set the date and the time.
    SetDateTime,
    /// The network and its settings.
    Network,
    /// What the network is carrying.
    NetworkActivity,
    /// Bring the task's window forward.
    SwitchTo,
    /// Pause it.
    Pause,
    /// Carry on with it.
    Resume,
    /// Give it less of the processor.
    LowerPriority,
    /// Read what it has logged.
    Logs,
    /// End it now, whatever it is doing.
    ForceQuit,
    /// Play it.
    Play,
    /// Play in a shuffled order.
    Shuffle,
    /// The ways of playing again.
    Repeat,
    /// Do not play again.
    RepeatOff,
    /// Play the whole list again.
    RepeatAll,
    /// Play the one track again.
    RepeatOne,
    /// Even out loudness between tracks.
    Loudness,
    /// Where the sound plays.
    AudioOutput,
    /// Start a new game.
    NewGame,
    /// The easiest level.
    LevelEasy,
    /// The middle level.
    LevelMedium,
    /// The hardest level.
    LevelHard,
    /// Allow question marks.
    QuestionMarks,
    /// Let the companion out onto the desktop.
    LetOut,
    /// Bring the companion home.
    BringHome,
}

impl AppMenuSymbol {
    /// Every symbol, in wire order: the symbol with wire value `n` is
    /// `ALL[n - 1]`.
    pub const ALL: [Self; 178] = [
        Self::Open,
        Self::OpenWith,
        Self::OpenFolder,
        Self::Application,
        Self::NewWindow,
        Self::NewDocument,
        Self::NewFolder,
        Self::Save,
        Self::SaveAs,
        Self::Close,
        Self::Quit,
        Self::Info,
        Self::Properties,
        Self::Settings,
        Self::File,
        Self::Edit,
        Self::View,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::SelectAll,
        Self::SelectNone,
        Self::Rename,
        Self::Duplicate,
        Self::Delete,
        Self::Remove,
        Self::Clear,
        Self::Undo,
        Self::Redo,
        Self::Indent,
        Self::Outdent,
        Self::Comment,
        Self::Overwrite,
        Self::Find,
        Self::Replace,
        Self::FindNext,
        Self::FindPrevious,
        Self::GoTo,
        Self::NextProblem,
        Self::ViewText,
        Self::ViewHex,
        Self::ViewData,
        Self::ViewCode,
        Self::InstructionSet,
        Self::Syntax,
        Self::TabWidth,
        Self::LineEndings,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ZoomFit,
        Self::FitWidth,
        Self::ZoomActual,
        Self::ZoomLevel,
        Self::TextLarger,
        Self::TextSmaller,
        Self::TextActual,
        Self::RotateLeft,
        Self::RotateRight,
        Self::RotateHalf,
        Self::FlipHorizontal,
        Self::FlipVertical,
        Self::Grid,
        Self::PixelGrid,
        Self::Panes,
        Self::ResetPanes,
        Self::Display,
        Self::LargeIcons,
        Self::SmallIcons,
        Self::List,
        Self::Refresh,
        Self::SortByName,
        Self::SortByKind,
        Self::SortBySize,
        Self::SortByDate,
        Self::ArrangeLeft,
        Self::ArrangeRight,
        Self::Wallpaper,
        Self::Shortcut,
        Self::Visible,
        Self::Hidden,
        Self::Home,
        Self::Folder,
        Self::Drive,
        Self::Previous,
        Self::Next,
        Self::MoveUp,
        Self::MoveDown,
        Self::Layers,
        Self::LayerNew,
        Self::LayerAbove,
        Self::LayerBelow,
        Self::LayerRaise,
        Self::LayerLower,
        Self::MergeDown,
        Self::Flatten,
        Self::Image,
        Self::Resize,
        Self::CanvasSize,
        Self::InvertColours,
        Self::ColourModel,
        Self::MaskAdd,
        Self::MaskRemove,
        Self::FillSelection,
        Self::CropToSelection,
        Self::PutDown,
        Self::Erase,
        Self::Colours,
        Self::ColourPrimary,
        Self::ColourSecondary,
        Self::ColourSwap,
        Self::ColourReset,
        Self::PickColour,
        Self::Adjust,
        Self::Brightness,
        Self::HueSaturation,
        Self::ColourBalance,
        Self::Levels,
        Self::Curves,
        Self::WhiteBalance,
        Self::Posterise,
        Self::Threshold,
        Self::Desaturate,
        Self::Blur,
        Self::Sharpen,
        Self::Pixelate,
        Self::Noise,
        Self::FindEdges,
        Self::Pages,
        Self::Tools,
        Self::ToolSelect,
        Self::ToolPencil,
        Self::ToolBrush,
        Self::ToolAirbrush,
        Self::ToolEraser,
        Self::ToolClone,
        Self::ToolFill,
        Self::ToolGradient,
        Self::ToolEyedropper,
        Self::ToolText,
        Self::ToolLine,
        Self::ToolRectangle,
        Self::ToolEllipse,
        Self::ToolPolygon,
        Self::ToolCrop,
        Self::ToolHand,
        Self::ToolZoom,
        Self::SystemMonitor,
        Self::TaskShell,
        Self::LockScreen,
        Self::SwitchUser,
        Self::LogOut,
        Self::Restart,
        Self::ShutDown,
        Self::Clock,
        Self::SetDateTime,
        Self::Network,
        Self::NetworkActivity,
        Self::SwitchTo,
        Self::Pause,
        Self::Resume,
        Self::LowerPriority,
        Self::Logs,
        Self::ForceQuit,
        Self::Play,
        Self::Shuffle,
        Self::Repeat,
        Self::RepeatOff,
        Self::RepeatAll,
        Self::RepeatOne,
        Self::Loudness,
        Self::AudioOutput,
        Self::NewGame,
        Self::LevelEasy,
        Self::LevelMedium,
        Self::LevelHard,
        Self::QuestionMarks,
        Self::LetOut,
        Self::BringHome,
    ];

    /// The symbol's wire value, never zero.
    #[must_use]
    pub const fn wire(self) -> u16 {
        self as u16
    }

    /// The symbol a wire value names.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for zero or a value past the vocabulary.
    pub const fn from_wire(raw: u16) -> Result<Self, Errno> {
        let Some(index) = raw.checked_sub(1) else {
            return Err(Errno::OutOfRange);
        };
        let index = index as usize;
        if index < Self::ALL.len() {
            Ok(Self::ALL[index])
        } else {
            Err(Errno::OutOfRange)
        }
    }
}

#[cfg(test)]
#[path = "symbol_tests.rs"]
mod tests;
