//! A machine's configuration as it crosses to the worker: every field of a
//! [`BoardConfig`], and a fixed disk's drive and low-level format.
//!
//! Decoding checks only the shape — each choice one the type has, each count
//! within its bound. Whether the choices go together is the board's own
//! question, which [`crate::board::Board::new`] answers.

use super::{flag, nth, number};
use crate::board::{
    BoardConfig, EmsConfig, FixedAdapter, FloppyAdapter, ParallelPort, PortsConfig, SerialPort,
    SoundCard, SoundConfig,
};
use crate::chips::ppi::Wiring;
use crate::cpu::CpuModel;
use crate::ports::parallel::Device;
use crate::ports::serial::{Model, Plug};
use crate::storage::fdc::{DriveKind, Interface};
use crate::storage::hdc::{DiskType, LowLevel, Recording, MAX_TRACKS};
use crate::time::{CpuClock, Hertz};
use crate::video::monitor::{Monitor, Phosphor};
use crate::video::Adapter;
use alloc::vec::Vec;
use tairix_sandbox::wire::{Reader, Room, WireError, Writer};

/// Bytes a [`BoardConfig`] takes at most.
pub const CONFIG_BYTES: usize = 128;

/// Bytes a fixed disk's drive and low-level format take at most.
pub const FIXED_BYTES: usize = 24 + 4 * MAX_TRACKS;

fn put_option<R: Room, T>(
    w: &mut Writer<R>,
    value: Option<T>,
    put: impl FnOnce(&mut Writer<R>, T),
) {
    match value {
        None => w.u8(0),
        Some(value) => {
            w.u8(1);
            put(w, value);
        }
    }
}

fn option<'a, T>(
    r: &mut Reader<'a>,
    take: impl FnOnce(&mut Reader<'a>) -> Result<T, WireError>,
) -> Result<Option<T>, WireError> {
    if flag(r)? {
        take(r).map(Some)
    } else {
        Ok(None)
    }
}

const WIRINGS: [Wiring; 2] = [Wiring::Pc, Wiring::Xt];
const PHOSPHORS: [Phosphor; 3] = [Phosphor::Green, Phosphor::Amber, Phosphor::White];
const INTERFACES: [Interface; 2] = [Interface::Standard, Interface::HighDensity];
const DRIVES: [DriveKind; 4] = [
    DriveKind::Dd525,
    DriveKind::Hd525,
    DriveKind::Dd35,
    DriveKind::Hd35,
];
const RECORDINGS: [Recording; 2] = [Recording::Mfm, Recording::Rll];
const UARTS: [Model; 2] = [Model::Ins8250, Model::Ns16450];
const PLUGS: [Plug; 2] = [Plug::Nothing, Plug::Mouse];
const PARALLEL_DEVICES: [Device; 4] = [
    Device::Nothing,
    Device::Covox,
    Device::SoundSource,
    Device::Printer,
];

fn put_monitor<R: Room>(w: &mut Writer<R>, monitor: Monitor) {
    match monitor {
        Monitor::Colour => w.raw(&[0, 0]),
        Monitor::Composite => w.raw(&[1, 0]),
        Monitor::Mono(phosphor) => w.raw(&[2, number(&PHOSPHORS, &phosphor)]),
    }
}

fn monitor(r: &mut Reader<'_>) -> Result<Monitor, WireError> {
    let (kind, phosphor) = (r.u8()?, r.u8()?);
    match (kind, phosphor) {
        (0, 0) => Ok(Monitor::Colour),
        (1, 0) => Ok(Monitor::Composite),
        (2, n) => nth(&PHOSPHORS, n).map(Monitor::Mono),
        _ => Err(WireError::Malformed),
    }
}

fn put_adapter<R: Room>(w: &mut Writer<R>, adapter: Adapter) {
    match adapter {
        Adapter::Mda(phosphor) => w.raw(&[0, number(&PHOSPHORS, &phosphor), 0]),
        Adapter::Hercules(phosphor) => w.raw(&[1, number(&PHOSPHORS, &phosphor), 0]),
        Adapter::Cga(m) => {
            w.u8(2);
            put_monitor(w, m);
        }
        Adapter::Ega(m) => {
            w.u8(3);
            put_monitor(w, m);
        }
        Adapter::Vga(m) => {
            w.u8(4);
            put_monitor(w, m);
        }
    }
}

fn adapter(r: &mut Reader<'_>) -> Result<Adapter, WireError> {
    match r.u8()? {
        kind @ (0 | 1) => {
            let phosphor = nth(&PHOSPHORS, r.u8()?)?;
            if r.u8()? != 0 {
                return Err(WireError::Malformed);
            }
            Ok(if kind == 0 {
                Adapter::Mda(phosphor)
            } else {
                Adapter::Hercules(phosphor)
            })
        }
        2 => monitor(r).map(Adapter::Cga),
        3 => monitor(r).map(Adapter::Ega),
        4 => monitor(r).map(Adapter::Vga),
        _ => Err(WireError::Malformed),
    }
}

/// Append a processor's clock.
pub fn put_cpu_clock<R: Room>(w: &mut Writer<R>, clock: CpuClock) {
    let (num, den) = clock.rate().parts();
    w.u64(num);
    w.u64(den);
}

/// A processor's clock as [`put_cpu_clock`] wrote it.
///
/// # Errors
///
/// [`WireError`] for a truncated rate, or one no processor is clocked at.
pub fn cpu_clock(r: &mut Reader<'_>) -> Result<CpuClock, WireError> {
    let (num, den) = (r.u64()?, r.u64()?);
    Hertz::ratio(num, den)
        .and_then(CpuClock::new)
        .ok_or(WireError::Malformed)
}

/// Append `config`.
pub fn put_config<R: Room>(w: &mut Writer<R>, config: &BoardConfig) {
    w.u8(number(&CpuModel::ALL, &config.cpu));
    put_cpu_clock(w, config.clock);
    w.u8(number(&WIRINGS, &config.wiring));
    w.u16(config.memory_kib);
    for adapter in config.video {
        put_option(w, adapter, put_adapter);
    }
    put_option(w, config.floppy, |w, floppy| {
        w.u8(number(&INTERFACES, &floppy.interface));
        for drive in floppy.drives {
            w.u8(drive.map_or(0, |kind| number(&DRIVES, &kind).saturating_add(1)));
        }
    });
    put_option(w, config.fixed, |w, fixed| {
        w.u8(number(&RECORDINGS, &fixed.recording));
        w.u8(fixed.jumpers);
    });
    w.u32(config.sound.stream_rate);
    match config.sound.card {
        None => w.u8(0),
        Some(SoundCard::AdLib) => w.u8(1),
        Some(SoundCard::Blaster { base, irq }) => {
            w.u8(2);
            w.u16(base);
            w.u8(irq);
        }
    }
    for serial in config.ports.serial {
        put_option(w, serial, |w, port| {
            w.u8(number(&UARTS, &port.model));
            w.u8(number(&PLUGS, &port.device));
        });
    }
    for parallel in config.ports.parallel {
        put_option(w, parallel, |w, port| {
            w.u16(port.base);
            w.u8(number(&PARALLEL_DEVICES, &port.device));
        });
    }
    w.u8(u8::from(config.ports.game));
    w.u8(u8::from(config.ports.clock));
    put_option(w, config.ems, |w, ems| {
        w.u16(ems.port);
        w.u32(ems.frame);
        w.u16(ems.pages);
    });
    w.u8(u8::from(config.coprocessor));
}

/// A configuration as [`put_config`] wrote it.
///
/// # Errors
///
/// [`WireError`] for a field no configuration has.
pub fn config(r: &mut Reader<'_>) -> Result<BoardConfig, WireError> {
    let cpu = nth(&CpuModel::ALL, r.u8()?)?;
    let clock = cpu_clock(r)?;
    let wiring = nth(&WIRINGS, r.u8()?)?;
    let memory_kib = r.u16()?;
    let video = [option(r, adapter)?, option(r, adapter)?];
    let floppy = option(r, |r| {
        let interface = nth(&INTERFACES, r.u8()?)?;
        let mut drives = [None; 4];
        for drive in &mut drives {
            *drive = match r.u8()? {
                0 => None,
                n => Some(nth(&DRIVES, n - 1)?),
            };
        }
        Ok(FloppyAdapter { interface, drives })
    })?;
    let fixed = option(r, |r| {
        Ok(FixedAdapter {
            recording: nth(&RECORDINGS, r.u8()?)?,
            jumpers: r.u8()?,
        })
    })?;
    let stream_rate = r.u32()?;
    let card = match r.u8()? {
        0 => None,
        1 => Some(SoundCard::AdLib),
        2 => Some(SoundCard::Blaster {
            base: r.u16()?,
            irq: r.u8()?,
        }),
        _ => return Err(WireError::Malformed),
    };
    let mut serial = [None; 4];
    for port in &mut serial {
        *port = option(r, |r| {
            Ok(SerialPort {
                model: nth(&UARTS, r.u8()?)?,
                device: nth(&PLUGS, r.u8()?)?,
            })
        })?;
    }
    let mut parallel = [None; 3];
    for port in &mut parallel {
        *port = option(r, |r| {
            Ok(ParallelPort {
                base: r.u16()?,
                device: nth(&PARALLEL_DEVICES, r.u8()?)?,
            })
        })?;
    }
    let ports = PortsConfig {
        serial,
        parallel,
        game: flag(r)?,
        clock: flag(r)?,
    };
    let ems = option(r, |r| {
        Ok(EmsConfig {
            port: r.u16()?,
            frame: r.u32()?,
            pages: r.u16()?,
        })
    })?;
    let coprocessor = flag(r)?;
    Ok(BoardConfig {
        cpu,
        clock,
        wiring,
        memory_kib,
        video,
        floppy,
        fixed,
        sound: SoundConfig { stream_rate, card },
        ports,
        ems,
        coprocessor,
    })
}

/// Append a fixed disk's drive and low-level format.
pub fn put_fixed<R: Room>(w: &mut Writer<R>, kind: &DiskType, format: &LowLevel) {
    w.u16(kind.cylinders);
    w.u8(kind.heads);
    w.u64(kind.track_to_track);
    w.u64(kind.average_seek);
    put_low_level(w, format);
}

/// A fixed disk's drive and low-level format as [`put_fixed`] wrote them.
///
/// # Errors
///
/// [`WireError`] for a truncated field or more bad tracks than any drive has.
pub fn fixed(r: &mut Reader<'_>) -> Result<(DiskType, LowLevel), WireError> {
    let kind = DiskType {
        cylinders: r.u16()?,
        heads: r.u8()?,
        track_to_track: r.u64()?,
        average_seek: r.u64()?,
    };
    Ok((kind, low_level(r)?))
}

/// Append a low-level format.
pub fn put_low_level<R: Room>(w: &mut Writer<R>, format: &LowLevel) {
    w.u8(format.interleave);
    let listed = &format.bad_tracks[..format.bad_tracks.len().min(MAX_TRACKS)];
    w.u32(u32::try_from(listed.len()).unwrap_or(u32::MAX));
    for &track in listed {
        w.u32(track);
    }
}

/// A low-level format as [`put_low_level`] wrote it.
///
/// # Errors
///
/// [`WireError`] for a truncated field or more bad tracks than any drive has.
pub fn low_level(r: &mut Reader<'_>) -> Result<LowLevel, WireError> {
    let interleave = r.u8()?;
    let count = usize::try_from(r.u32()?).map_err(|_| WireError::Malformed)?;
    if count > MAX_TRACKS || count.saturating_mul(4) > r.remaining() {
        return Err(WireError::Malformed);
    }
    let mut bad_tracks = Vec::new();
    bad_tracks
        .try_reserve_exact(count)
        .map_err(|_| WireError::Malformed)?;
    for _ in 0..count {
        bad_tracks.push(r.u32()?);
    }
    Ok(LowLevel {
        interleave,
        bad_tracks,
    })
}
