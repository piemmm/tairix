//! The module inventory (`plans/RISCOS-EMULATOR.md` §4.3): every module of
//! RISC OS 5.30's ROM for IOMD, in the ROM's order, with its version, as
//! `OS_Module` 20 reads them on genuine RISC OS 5, and what the emulator
//! holds of each. A module's place is its chunk number, which names its
//! unplug bit in CMOS.

/// What the emulator holds of a ROM module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    /// Provided whole, with its version's interface, once its item lands.
    Provided,
    /// Provided, its effect outside the guest relayed to the desktop.
    Bridged,
    /// Not provided: it serves hardware the emulator does not have, or a part
    /// of TAIRiX takes its place. No lookup finds it.
    Absent,
}

/// A module of the ROM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rom {
    /// Its title.
    pub title: &'static str,
    /// Its version, in hundredths.
    pub version: u16,
    /// What the emulator holds of it.
    pub held: Held,
}

const fn rom(title: &'static str, version: u16, held: Held) -> Rom {
    Rom {
        title,
        version,
        held,
    }
}

/// `n`, below ten thousand, in binary-coded decimal.
const fn bcd(n: u16) -> u32 {
    let n = n as u32;
    ((n / 1000 % 10) << 12) | ((n / 100 % 10) << 8) | ((n / 10 % 10) << 4) | (n % 10)
}

impl Rom {
    /// Its version in binary-coded decimal.
    #[must_use]
    pub const fn bcd(&self) -> u32 {
        (bcd(self.version / 100) << 16) | (bcd(self.version % 100) << 8)
    }

    /// Whether the emulator provides it.
    #[must_use]
    pub const fn provided(&self) -> bool {
        !matches!(self.held, Held::Absent)
    }
}

/// The ROM's modules, in its order.
pub static INVENTORY: [Rom; 121] = [
    rom("UtilityModule", 530, Held::Bridged),
    rom("Podule", 172, Held::Absent),
    rom("FileSwitch", 292, Held::Bridged),
    rom("ResourceFS", 26, Held::Provided),
    rom("TerritoryManager", 58, Held::Provided),
    rom("Messages", 112, Held::Provided),
    rom("MessageTrans", 49, Held::Provided),
    rom("UK", 64, Held::Provided),
    rom("WindowManager", 587, Held::Bridged),
    rom("TaskManager", 154, Held::Bridged),
    rom("Desktop", 277, Held::Provided),
    rom("SharedCLibrary", 623, Held::Provided),
    rom("ScreenModes", 74, Held::Provided),
    rom("VIDC20Video", 9, Held::Absent),
    rom("Mouse", 11, Held::Absent),
    rom("PS2Driver", 40, Held::Absent),
    rom("ADFSFiler", 116, Held::Absent),
    rom("BASIC", 185, Held::Provided),
    rom("BASIC64", 185, Held::Provided),
    rom("BlendTable", 2, Held::Provided),
    rom("BufferManager", 39, Held::Provided),
    rom("ColourTrans", 197, Held::Provided),
    rom("Debugger", 208, Held::Provided),
    rom("DeviceFS", 73, Held::Provided),
    rom("DisplayManager", 44, Held::Bridged),
    rom("DMAManager", 23, Held::Absent),
    rom("DragASprite", 21, Held::Provided),
    rom("DragAnObject", 10, Held::Provided),
    rom("Draw", 122, Held::Provided),
    rom("BBCEconet", 20, Held::Absent),
    rom("FileCore", 378, Held::Provided),
    rom("ADFS", 356, Held::Absent),
    rom("RamFS", 233, Held::Provided),
    rom("Filer", 249, Held::Bridged),
    rom("FilerSWIs", 5, Held::Bridged),
    rom("FSLock", 126, Held::Absent),
    rom("FontManager", 380, Held::Bridged),
    rom("FPEmulator", 439, Held::Provided),
    rom("Free", 42, Held::Provided),
    rom("Hourglass", 219, Held::Bridged),
    rom("IIC", 18, Held::Absent),
    rom("International", 170, Held::Provided),
    rom("InternationalKeyboard", 100, Held::Provided),
    rom("InverseTable", 18, Held::Provided),
    rom("Joystick", 29, Held::Absent),
    rom("NetFS", 600, Held::Absent),
    rom("NetFiler", 87, Held::Absent),
    rom("NetPrint", 561, Held::Absent),
    rom("NetStatus", 209, Held::Absent),
    rom("Obey", 41, Held::Provided),
    rom("ParallelDeviceDriver", 79, Held::Absent),
    rom("Pinboard", 107, Held::Bridged),
    rom("PipeFS", 25, Held::Provided),
    rom("RAMFSFiler", 41, Held::Bridged),
    rom("ResourceFiler", 20, Held::Bridged),
    rom("ROMFonts", 78, Held::Provided),
    rom("RTC", 2, Held::Absent),
    rom("ScreenBlanker", 234, Held::Bridged),
    rom("ScrSaver", 14, Held::Bridged),
    rom("SerialDeviceDriver", 48, Held::Absent),
    rom("SerialDeviceSupport", 26, Held::Absent),
    rom("SerialMouse", 10, Held::Absent),
    rom("ShellCLI", 39, Held::Bridged),
    rom("SoundDMA", 172, Held::Bridged),
    rom("SoundChannels", 143, Held::Provided),
    rom("SoundScheduler", 134, Held::Provided),
    rom("SpriteExtend", 186, Held::Provided),
    rom("SpriteUtils", 113, Held::Provided),
    rom("Squash", 31, Held::Provided),
    rom("SuperSample", 16, Held::Provided),
    rom("SystemDevices", 134, Held::Provided),
    rom("TaskWindow", 85, Held::Provided),
    rom("WindowUtils", 253, Held::Provided),
    rom("FilterManager", 30, Held::Provided),
    rom("WaveSynth", 125, Held::Provided),
    rom("StringLib", 120, Held::Provided),
    rom("Percussion", 119, Held::Provided),
    rom("SharedSound", 120, Held::Provided),
    rom("Filer_Action", 64, Held::Bridged),
    rom("DOSFS", 115, Held::Absent),
    rom("ColourPicker", 59, Held::Provided),
    rom("DrawFile", 162, Held::Provided),
    rom("BootCommands", 153, Held::Provided),
    rom("WindowScroll", 2, Held::Provided),
    rom("MbufManager", 30, Held::Absent),
    rom("Internet", 567, Held::Bridged),
    rom("Resolver", 74, Held::Provided),
    rom("Net", 627, Held::Absent),
    rom("BootNet", 97, Held::Absent),
    rom("Freeway", 42, Held::Absent),
    rom("ShareFS", 362, Held::Absent),
    rom("MimeMap", 19, Held::Provided),
    rom("LanManFS", 270, Held::Absent),
    rom("DHCP", 27, Held::Absent),
    rom("!Edit", 175, Held::Absent),
    rom("!Draw", 146, Held::Absent),
    rom("!Paint", 254, Held::Absent),
    rom("!Alarm", 296, Held::Absent),
    rom("!Chars", 206, Held::Absent),
    rom("!Help", 327, Held::Absent),
    rom("Toolbox", 160, Held::Provided),
    rom("Window", 181, Held::Provided),
    rom("ToolAction", 38, Held::Provided),
    rom("Menu", 42, Held::Provided),
    rom("Iconbar", 123, Held::Provided),
    rom("ColourDbox", 23, Held::Provided),
    rom("ColourMenu", 23, Held::Provided),
    rom("DCS", 114, Held::Provided),
    rom("FileInfo", 21, Held::Provided),
    rom("FontDbox", 19, Held::Provided),
    rom("FontMenu", 26, Held::Provided),
    rom("PrintDbox", 18, Held::Provided),
    rom("ProgInfo", 19, Held::Provided),
    rom("SaveAs", 21, Held::Provided),
    rom("Scale", 18, Held::Provided),
    rom("TextGadgets", 45, Held::Provided),
    rom("CDFSDriver", 242, Held::Absent),
    rom("CDFSSoftATAPI", 144, Held::Absent),
    rom("CDFS", 263, Held::Absent),
    rom("CDFSFiler", 253, Held::Absent),
    rom("UnSqueezeAIF", 10, Held::Provided),
];
