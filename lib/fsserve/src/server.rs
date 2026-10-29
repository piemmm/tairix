//! One call answered: the protocol dispatched onto the driver traits.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    AllocationClass, DirVisit, FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity,
    FilesystemStats, FilesystemWrite, NodeId,
};
use tairix_abi::fs::{is_component, FS_ATTR_VALUE_MAX, FS_SYMLINK_MAX};
use tairix_abi::fsserve::{
    decode_request, encode_reply, request_op, Answer, DirEnd, DirReply, Request, Span,
};
use tairix_abi::{DriverError, Errno};

use crate::host::{Clock, ServedHost};

/// Everything a served driver is: the filesystem driver traits an in-kernel
/// driver implements, so one driver crate serves either way.
pub trait ServedFilesystem:
    FilesystemRead + FilesystemWrite + FilesystemSecurity + FilesystemStats + FilesystemAttrsProvider
{
}

impl<T> ServedFilesystem for T where
    T: FilesystemRead
        + FilesystemWrite
        + FilesystemSecurity
        + FilesystemStats
        + FilesystemAttrsProvider
{
}

/// The data window a served volume's file data moves through, reached a
/// call's span at a time.
///
/// The kernel stages and collects every other lane's span while one call is
/// in service, so a server is lent the span its call names and never a borrow
/// of the whole window.
pub trait Window {
    /// The bytes of `span`, the call in service's own, or `None` where the
    /// window does not hold it.
    fn span(&mut self, span: Span) -> Option<&mut [u8]>;
}

/// A buffer the server owns outright, which nothing else writes.
impl Window for [u8] {
    fn span(&mut self, span: Span) -> Option<&mut [u8]> {
        self.get_mut(span.range())
    }
}

/// The largest value a non-window answer copies: a link target, which is
/// longer than any attribute value or key.
const SCRATCH_LEN: usize = if FS_SYMLINK_MAX > FS_ATTR_VALUE_MAX {
    FS_SYMLINK_MAX
} else {
    FS_ATTR_VALUE_MAX
};

/// A served volume's driver, answering the kernel's calls one at a time.
pub struct Server<'h, F, C> {
    fs: F,
    host: &'h ServedHost<C>,
    /// Where a link target or an attribute lands before its reply is built,
    /// allocated once.
    scratch: Vec<u8>,
}

impl<'h, F: ServedFilesystem, C: Clock> Server<'h, F, C> {
    /// Serve `fs`, which reports to `host`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] if the scratch buffer cannot be allocated.
    pub fn new(fs: F, host: &'h ServedHost<C>) -> Result<Self, Errno> {
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(SCRATCH_LEN)
            .map_err(|_| Errno::OutOfMemory)?;
        scratch.resize(SCRATCH_LEN, 0);
        Ok(Self { fs, host, scratch })
    }

    /// Answer the call `request` into `out`, its file data moving through
    /// `window`, returning the reply's length.
    ///
    /// A request the kernel could not have sent is refused as out of range;
    /// one that names no operation gets an empty reply, which the kernel
    /// refuses in turn. Either way the driver is never reached.
    pub fn answer<W: Window + ?Sized>(
        &mut self,
        request: &[u8],
        window: &mut W,
        out: &mut [u8],
    ) -> usize {
        let Ok((class, request)) = decode_request(request) else {
            return request_op(request).map_or(0, |op| {
                encode_reply(op, self.host.state(), Err(DriverError::OutOfRange), out).unwrap_or(0)
            });
        };
        // The kernel scopes a class to one operation.
        self.fs.set_allocation_class(class);
        let len = match request {
            Request::ReadDir { dir, cursor, after } => self.list(dir, cursor, after, out),
            _ => self.call(&request, window, out),
        };
        self.fs.set_allocation_class(AllocationClass::Ordinary);
        len
    }

    /// Publish the driver's open transaction: its commit timer fired.
    ///
    /// # Errors
    ///
    /// The driver's flush error.
    pub fn publish(&mut self) -> Result<(), DriverError> {
        self.fs.flush()
    }

    /// The driver served.
    pub fn filesystem(&mut self) -> &mut F {
        &mut self.fs
    }

    fn call<W: Window + ?Sized>(
        &mut self,
        request: &Request<'_>,
        window: &mut W,
        out: &mut [u8],
    ) -> usize {
        let op = request.op();
        let result = run(&mut self.fs, &mut self.scratch, request, window);
        let state = self.host.state();
        // A value the codec refuses is a driver reporting what no volume
        // holds: answered as the fault it is.
        encode_reply(op, state, result, out)
            .or_else(|_| encode_reply(op, state, Err(DriverError::DeviceFault), out))
            .unwrap_or(0)
    }

    fn list(&mut self, dir: NodeId, cursor: u64, after: &[u8], out: &mut [u8]) -> usize {
        let Ok(mut reply) = DirReply::new(out) else {
            return 0;
        };
        let mut stopped = None;
        let listed = self.fs.read_dir(dir, cursor, after, &mut |entry, name| {
            // A name no path can spell fails the listing closed, as it does
            // from an in-kernel driver, rather than being listed around.
            if !is_component(name) || entry.node == NodeId::NONE {
                stopped = Some(DirEnd::Failed(DriverError::DeviceFault));
                return DirVisit::Stop;
            }
            if reply.push(entry, name).is_ok() {
                DirVisit::Take
            } else {
                stopped = Some(DirEnd::More);
                DirVisit::Stop
            }
        });
        let end = match (listed, stopped) {
            (Err(err), _) => DirEnd::Failed(err),
            (Ok(()), Some(end)) => end,
            (Ok(()), None) => DirEnd::Ended,
        };
        reply.finish(end, self.host.state()).unwrap_or(0)
    }
}

/// Run `request` against `fs`, a link target or attribute landing in
/// `scratch` and file data moving through `window`.
#[allow(clippy::too_many_lines)] // One arm per operation; splitting it would scatter the dispatch.
fn run<'s, F: ServedFilesystem, W: Window + ?Sized>(
    fs: &mut F,
    scratch: &'s mut [u8],
    request: &Request<'_>,
    window: &mut W,
) -> Result<Answer<'s>, DriverError> {
    Ok(match *request {
        Request::NodeInfo { node } => Answer::Info(fs.node_info(node)?),
        Request::Lookup { dir, name } => Answer::Node(fs.lookup(dir, name)?),
        Request::Parent { dir } => Answer::Node(fs.parent(dir)?),
        Request::ReadAt { file, offset, span } => {
            let slot = window.span(span).ok_or(DriverError::OutOfRange)?;
            Answer::Count(counted(fs.read_at(file, offset, slot)?, span)?)
        }
        Request::ReadLink { link, cap } => {
            let out = buffer(scratch, cap)?;
            let len = fs.read_link(link, out)?;
            Answer::Target(written(out, len)?)
        }
        Request::SeekRegion {
            file,
            offset,
            region,
        } => Answer::Region(fs.seek_region(file, offset, region)?),
        // A listing streams into its reply; `Server::list` answers it.
        Request::ReadDir { .. } => return Err(DriverError::OutOfRange),
        Request::Pin { node } => Answer::Node(fs.pin(node)?),
        Request::Unpin { pinned } => Answer::Unpinned(fs.unpin(pinned)?),
        Request::ReleasePins => {
            fs.release_pins()?;
            Answer::Done
        }
        Request::Create {
            dir,
            name,
            kind,
            record,
        } => Answer::Node(fs.create(dir, name, kind, &record)?),
        Request::CreateLink {
            dir,
            name,
            target,
            record,
        } => Answer::Node(fs.create_link(dir, name, target, &record)?),
        Request::Link { dir, name, node } => {
            fs.link(dir, name, node)?;
            Answer::Done
        }
        Request::WriteAt { file, offset, span } => {
            let staged = window.span(span).ok_or(DriverError::OutOfRange)?;
            Answer::Count(counted(fs.write_at(file, offset, staged)?, span)?)
        }
        Request::Truncate { file, size } => {
            fs.truncate(file, size)?;
            Answer::Done
        }
        Request::Punch { file, offset, len } => {
            fs.punch(file, offset, len)?;
            Answer::Done
        }
        Request::Remove { dir, name } => {
            fs.remove(dir, name)?;
            Answer::Done
        }
        Request::Rename {
            src_dir,
            src_name,
            dst_dir,
            dst_name,
        } => {
            fs.rename(src_dir, src_name, dst_dir, dst_name)?;
            Answer::Done
        }
        Request::Flush => {
            fs.flush()?;
            Answer::Done
        }
        Request::Security { node } => Answer::Security(fs.security(node)?),
        Request::SetSecurity { node, security } => {
            fs.set_security(node, security)?;
            Answer::Done
        }
        Request::RecordsOwnership { node } => Answer::Owned(fs.records_ownership(node)?),
        Request::Stats => Answer::Stats(fs.stats()?),
        Request::GetAttr { node, key, cap } => {
            let attrs = fs.attrs_fs().ok_or(DriverError::Unsupported)?;
            let out = buffer(scratch, cap)?;
            match attrs.get_attr(node, key, out)? {
                None => Answer::Attr(None),
                Some(len) => Answer::Attr(Some(written(out, len)?)),
            }
        }
        Request::SetAttr { node, key, value } => {
            let attrs = fs.attrs_fs().ok_or(DriverError::Unsupported)?;
            attrs.set_attr(node, key, value)?;
            Answer::Done
        }
        Request::ListAttr { node, index, cap } => {
            let attrs = fs.attrs_fs().ok_or(DriverError::Unsupported)?;
            let out = buffer(scratch, cap)?;
            match attrs.list_attr(node, index, out)? {
                None => Answer::Attr(None),
                Some(len) => Answer::Attr(Some(written(out, len)?)),
            }
        }
        Request::RemoveAttr { node, key } => {
            let attrs = fs.attrs_fs().ok_or(DriverError::Unsupported)?;
            attrs.remove_attr(node, key)?;
            Answer::Done
        }
        // Calls are answered one at a time, so every earlier one is done.
        Request::Release { .. } => Answer::Done,
    })
}

/// The caller's `cap`-byte buffer, carved from `scratch`.
fn buffer(scratch: &mut [u8], cap: u32) -> Result<&mut [u8], DriverError> {
    scratch
        .get_mut(..cap as usize)
        .ok_or(DriverError::OutOfRange)
}

/// The `len` bytes a driver says it wrote into `out`: no more than it had.
fn written(out: &[u8], len: usize) -> Result<&[u8], DriverError> {
    out.get(..len).ok_or(DriverError::DeviceFault)
}

/// A driver's byte count, held to the span it moved data through.
fn counted(count: usize, span: Span) -> Result<u32, DriverError> {
    if count > span.size() {
        return Err(DriverError::DeviceFault);
    }
    u32::try_from(count).map_err(|_| DriverError::DeviceFault)
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
