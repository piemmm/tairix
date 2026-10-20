//! What a compositor presents through: a whole frame and the damage it
//! carries, and where the device has them, its hardware layers and cursor
//! plane.
//!
//! The driver's own trait, [`Display`](tairix_abi::driver::display::Display),
//! shows buffers the driver allocated; a presenter is what fills them. A
//! compositor names only these traits, never a driver's.

use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageRect, DisplayMode, DisplayPower,
};
use tairix_abi::DriverError;

/// A display a compositor presents finished frames to.
pub trait Presenter {
    /// The mode the frames are laid out in.
    fn mode(&self) -> DisplayMode;

    /// Whether a present may go now: none waits for the display's next
    /// frame start. Until it may, the display still reads the buffers the
    /// last present named, so nothing is drawn into them.
    fn ready(&self) -> bool;

    /// Present `frame`, laid out as [`Self::mode`], of which only `damage`
    /// changed since the previous present.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Busy`] while the previous frame waits for the
    ///   display's next frame start; the frame is owed again.
    /// * [`DriverError::Stale`] once the output moved off the mode.
    /// * [`DriverError::LengthOutOfRange`] for a malformed damage list.
    /// * [`DriverError::BufferTooSmall`] for a frame shorter than the mode.
    /// * [`DriverError::SeatRevoked`] or [`DriverError::PermissionDenied`]
    ///   once the seat is lost.
    fn present(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError>;

    /// Switch the display on or off.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the display has no power control,
    /// or the display's refusal.
    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError>;

    /// The display's hardware layers, if it has any.
    fn layers(&mut self) -> Option<&mut dyn LayerPresenter> {
        None
    }

    /// The display's cursor plane, if it has one.
    fn cursor(&mut self) -> Option<&mut dyn CursorPlane> {
        None
    }
}

/// The output frame beneath a layered present, as [`Presenter::present`]
/// takes it.
#[derive(Copy, Clone)]
pub struct Base<'a> {
    /// The frame, laid out as [`Presenter::mode`].
    pub frame: &'a [u8],
    /// What changed in it since the previous frame the display took.
    pub damage: &'a [DamageRect],
}

/// A display's hardware layer compositor: buffers a compositor bakes layers
/// into, composited by the device.
pub trait LayerPresenter {
    /// What the compositor can do.
    fn caps(&self) -> AccelCaps;

    /// Allocate a zeroed layer buffer of `width_px` × `height_px`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] past what the display lets one presenter
    /// hold, or the display's refusal.
    fn alloc(&mut self, width_px: u32, height_px: u32) -> Result<BufferId, DriverError>;

    /// Free layer buffer `id`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for a buffer this presenter does not hold.
    fn free(&mut self, id: BufferId) -> Result<(), DriverError>;

    /// Whether `id` is a layer buffer this presenter allocated: a compositor
    /// still holding one from a display since replaced finds it is not.
    fn holds(&self, id: BufferId) -> bool;

    /// The pixels of buffer `id` and the bytes from one of its rows to the
    /// next, to bake a layer into: premultiplied, in the mode's format.
    fn pixels(&mut self, id: BufferId) -> Option<(&mut [u8], u32)>;

    /// Show `layers`, bottom-most first, over `base` where there is one: the
    /// output frame [`Presenter::present`] would have shown, of which only
    /// its damage changed, laid beneath them as the whole output.
    ///
    /// Each layer's buffer is read until the next present is
    /// [`ready`](Presenter::ready), so a compositor bakes a changed layer
    /// into the other of two.
    ///
    /// # Errors
    ///
    /// As [`Presenter::present`]; [`DriverError::NotFound`] for a layer
    /// naming a buffer this presenter does not hold;
    /// [`DriverError::LengthOutOfRange`] or [`DriverError::Unsupported`] for a
    /// stack the compositor refuses; or [`DriverError::OutOfRange`] for one it
    /// cannot composite at once. A refused stack changes nothing on screen.
    fn present_layers(
        &mut self,
        base: Option<Base<'_>>,
        layers: &[AccelLayer],
    ) -> Result<(), DriverError>;
}

/// A display's cursor plane: the pointer drawn by the device, so moving it
/// presents no frame.
pub trait CursorPlane {
    /// The pointer image size the plane shows.
    fn caps(&self) -> CursorCaps;

    /// Show `image` — `width_px` × `height_px` premultiplied pixels in the
    /// mode's format, `stride_bytes` per row — as the pointer, its hot spot
    /// `(hot_x, hot_y)` pixels in from its top-left. The image is the
    /// device's once this returns.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for an image larger than
    /// [`Self::caps`], a short image, or a hot spot outside it; or the
    /// display's refusal.
    fn show(
        &mut self,
        image: &[u8],
        width_px: u32,
        height_px: u32,
        stride_bytes: u32,
        hot_x: u32,
        hot_y: u32,
    ) -> Result<(), DriverError>;

    /// Put the pointer's hot spot at `(x, y)` on the output.
    ///
    /// # Errors
    ///
    /// The display's refusal.
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), DriverError>;

    /// Take the pointer off the screen.
    ///
    /// # Errors
    ///
    /// The display's refusal.
    fn hide(&mut self) -> Result<(), DriverError>;

    /// Whether the plane shows an image this presenter gave it.
    fn showing(&self) -> bool;
}
