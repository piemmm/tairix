//! The scripted server's `IPC$`: the `srvsvc` pipe, and the one call it
//! answers, `NetrShareEnum`, listing the server's shares.
//!
//! A pipe is message mode: each PDU the server writes is one message, and a
//! read too short for it gets the start with `STATUS_BUFFER_OVERFLOW`, the
//! rest left for the next read.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{behind_header, empty_error, Fault, PeerConfig, DISK_SHARE};
use crate::file::create::{CreateAction, CreateRequest, CreateResponse, OplockLevel};
use crate::file::io::{
    CloseRequest, CloseResponse, ReadRequest, ReadResponse, WriteRequest, WriteResponse,
};
use crate::file::{FileAttributes, FileId, FileTimes};
use crate::header::Command;
use crate::ioctl::{IoctlRequest, IoctlResponse};
use crate::rpc::{
    read_share_enum_request, write_share_enum, Bind, BindAck, Pdu, ResponseFragment, ShareKind,
    ShareRecord, ACCEPTANCE, FSCTL_PIPE_TRANSCEIVE, MAX_FRAGMENT, NDR, PIPE, SHARE_ENUM, SPECIAL,
    SRVSVC,
};
use crate::status::NtStatus;
use crate::wire::Encoder;
use crate::SmbError;

/// `nca_s_op_rng_error`: the call names no operation the interface has.
const OPERATION_RANGE: u32 = 0x1c01_0002;

/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: u32 = 5;

/// A bind rejection's reason: `protocol_version_not_supported`.
const VERSION_NOT_SUPPORTED: u16 = 4;

/// A response fragment's fixed part, which its stub follows.
const FRAGMENT_FIXED: usize = 24;

/// The least fragment any end must receive (C706 §12.6.3.1).
const MIN_FRAGMENT: u16 = 1432;

/// What an extra share's remark says, long enough that a few dozen shares
/// span fragments.
const EXTRA_REMARK: &str = "One of many shares, listed to fill fragments";

#[derive(Debug)]
struct PipeOpen {
    file_id: FileId,
    /// The largest fragment the client receives, once it has bound.
    max_frag: usize,
    /// The messages written to the client and not yet read.
    waiting: VecDeque<Vec<u8>>,
    /// What a short read left of the message it began.
    rest: Vec<u8>,
}

/// The pipes open on `IPC$`.
#[derive(Debug)]
pub(super) struct Pipes {
    opens: Vec<PipeOpen>,
    next_id: u64,
}

impl Pipes {
    pub(super) const fn new() -> Self {
        Self {
            opens: Vec::new(),
            next_id: 0,
        }
    }

    /// Answer a request on `IPC$`: its status, and its body behind a
    /// header-sized gap.
    pub(super) fn request(
        &mut self,
        config: &PeerConfig,
        command: Command,
        message: &[u8],
    ) -> Result<(NtStatus, Vec<u8>), SmbError> {
        match command {
            Command::Create => self.create(message),
            Command::Write => {
                let write = WriteRequest::decode(message)?;
                let Some(open) = self.open(write.file_id) else {
                    return refused(NtStatus::FILE_CLOSED);
                };
                serve(config, open, write.data)?;
                let count = u32::try_from(write.data.len()).map_err(|_| SmbError::Protocol)?;
                answered(|enc| WriteResponse { count }.encode(enc))
            }
            Command::Read => {
                let read = ReadRequest::decode(message)?;
                let Some(open) = self.open(read.file_id) else {
                    return refused(NtStatus::FILE_CLOSED);
                };
                let length = usize::try_from(read.length).map_err(|_| SmbError::Protocol)?;
                let (status, data) = next_message(open, length)?;
                let body = behind_header(|enc| ReadResponse { data: &data }.encode(enc))?;
                Ok((status, body))
            }
            Command::Ioctl => {
                let request = IoctlRequest::decode(message)?;
                if request.ctl_code != FSCTL_PIPE_TRANSCEIVE {
                    return refused(NtStatus::NOT_SUPPORTED);
                }
                let Some(open) = self.open(request.file_id) else {
                    return refused(NtStatus::FILE_CLOSED);
                };
                serve(config, open, &request.input)?;
                let length =
                    usize::try_from(request.max_output_response).map_err(|_| SmbError::Protocol)?;
                let (status, output) = next_message(open, length)?;
                let response = IoctlResponse {
                    ctl_code: FSCTL_PIPE_TRANSCEIVE,
                    file_id: request.file_id,
                    output,
                };
                let body = behind_header(|enc| response.encode(enc))?;
                Ok((status, body))
            }
            Command::Close => {
                let close = CloseRequest::decode(message)?;
                let Some(at) = self
                    .opens
                    .iter()
                    .position(|open| open.file_id == close.file_id)
                else {
                    return refused(NtStatus::FILE_CLOSED);
                };
                self.opens.swap_remove(at);
                answered(|enc| CloseResponse { attributes: None }.encode(enc))
            }
            _ => refused(NtStatus::NOT_SUPPORTED),
        }
    }

    /// How many pipes are open.
    pub(super) fn open_count(&self) -> usize {
        self.opens.len()
    }

    fn open(&mut self, file_id: FileId) -> Option<&mut PipeOpen> {
        self.opens.iter_mut().find(|open| open.file_id == file_id)
    }

    fn create(&mut self, message: &[u8]) -> Result<(NtStatus, Vec<u8>), SmbError> {
        let request = CreateRequest::decode(message)?;
        if request.path.as_str() != PIPE {
            return refused(NtStatus::OBJECT_NAME_NOT_FOUND);
        }
        self.next_id += 1;
        let mut file_id = [0; 16];
        file_id[..8].copy_from_slice(&self.next_id.to_le_bytes());
        file_id[8..].copy_from_slice(&(!self.next_id).to_le_bytes());
        self.opens.push(PipeOpen {
            file_id,
            max_frag: MAX_FRAGMENT,
            waiting: VecDeque::new(),
            rest: Vec::new(),
        });
        let epoch = Time64::UNIX_EPOCH;
        let response = CreateResponse {
            oplock: OplockLevel::None,
            reparse_point: false,
            action: CreateAction::Opened,
            times: FileTimes {
                created: epoch,
                accessed: epoch,
                written: epoch,
                changed: epoch,
            },
            allocation_size: 4096,
            end_of_file: 0,
            attributes: FileAttributes::NORMAL,
            file_id,
            lease: None,
            durable_timeout_ms: None,
            maximal_access: None,
            disk_id: None,
        };
        answered(|enc| response.encode(enc))
    }
}

fn answered(encode: impl FnOnce(&mut Encoder)) -> Result<(NtStatus, Vec<u8>), SmbError> {
    Ok((NtStatus::SUCCESS, behind_header(encode)?))
}

fn refused(status: NtStatus) -> Result<(NtStatus, Vec<u8>), SmbError> {
    Ok((status, behind_header(empty_error)?))
}

/// The next `length` bytes of the message being read, and whether it goes on.
fn next_message(open: &mut PipeOpen, length: usize) -> Result<(NtStatus, Vec<u8>), SmbError> {
    if open.rest.is_empty() {
        open.rest = open.waiting.pop_front().ok_or(SmbError::Protocol)?;
    }
    let taken = length.min(open.rest.len());
    let data: Vec<u8> = open.rest.drain(..taken).collect();
    let status = if open.rest.is_empty() {
        NtStatus::SUCCESS
    } else {
        NtStatus::BUFFER_OVERFLOW
    };
    Ok((status, data))
}

/// Act on one PDU the client wrote, queueing what the server answers.
fn serve(config: &PeerConfig, open: &mut PipeOpen, pdu: &[u8]) -> Result<(), SmbError> {
    let mut enc = Encoder::new();
    match Pdu::decode(pdu)? {
        Pdu::Bind(bind) => answer_bind(config, open, &bind, &mut enc),
        Pdu::Request(request) if request.first && request.last && request.opnum == SHARE_ENUM => {
            read_share_enum_request(request.stub)?;
            let names: Vec<String> = (0..config.extra_shares)
                .map(|index| format!("share-{index:03}"))
                .collect();
            let mut records = Vec::from([
                ShareRecord {
                    name: DISK_SHARE,
                    share_type: ShareKind::Disk.code(),
                    remark: "The test share",
                },
                ShareRecord {
                    name: "IPC$",
                    share_type: ShareKind::Ipc.code() | SPECIAL,
                    remark: "IPC Service (tairix test peer)",
                },
            ]);
            records.extend(names.iter().map(|name| ShareRecord {
                name,
                share_type: ShareKind::Disk.code(),
                remark: EXTRA_REMARK,
            }));
            if config.fault == Fault::HostileShareName {
                records.push(ShareRecord {
                    name: r"..\outside",
                    share_type: ShareKind::Disk.code(),
                    remark: "",
                });
            }
            let status = if config.fault == Fault::DenyShareEnum {
                ACCESS_DENIED
            } else {
                0
            };
            let mut stub = Encoder::new();
            write_share_enum(&mut stub, &records, status);
            let stub = stub.finish()?;
            let per_fragment = open.max_frag - FRAGMENT_FIXED;
            let chunks: Vec<&[u8]> = stub.chunks(per_fragment).collect();
            for (index, chunk) in chunks.iter().enumerate() {
                let mut fragment = Encoder::new();
                Pdu::Response(ResponseFragment {
                    call_id: request.call_id,
                    first: index == 0,
                    last: index + 1 == chunks.len(),
                    context: request.context,
                    stub: chunk,
                })
                .encode(&mut fragment);
                open.waiting.push_back(fragment.finish()?);
            }
            return Ok(());
        }
        Pdu::Request(request) => Pdu::Fault {
            call_id: request.call_id,
            status: OPERATION_RANGE,
        }
        .encode(&mut enc),
        _ => return Err(SmbError::Protocol),
    }
    open.waiting.push_back(enc.finish()?);
    Ok(())
}

fn answer_bind(config: &PeerConfig, open: &mut PipeOpen, bind: &Bind, enc: &mut Encoder) {
    if config.fault == Fault::RefuseBind
        || bind.abstract_syntax != SRVSVC
        || bind.transfer_syntax != NDR
        || bind.max_recv_frag < MIN_FRAGMENT
    {
        Pdu::BindNak {
            call_id: bind.call_id,
            reason: VERSION_NOT_SUPPORTED,
        }
        .encode(enc);
        return;
    }
    open.max_frag = MAX_FRAGMENT.min(usize::from(bind.max_recv_frag));
    let frag = u16::try_from(open.max_frag).unwrap_or(u16::MAX);
    Pdu::BindAck(BindAck {
        call_id: bind.call_id,
        max_xmit_frag: frag,
        max_recv_frag: frag,
        assoc_group: 0x0001_2345,
        secondary_address: b"\\PIPE\\srvsvc\0",
        result: ACCEPTANCE,
        reason: 0,
        transfer_syntax: NDR,
    })
    .encode(enc);
}
