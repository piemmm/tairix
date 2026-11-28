# TAIRiX Bridge

TAIRiX Bridge is TAIRiX's compatibility layer: it runs other systems'
applications on the TAIRiX desktop, one subtree for each system. RISC OS is
the first, in `riscos/`, one crate per subsystem: `arm/`, the ARM processor;
`kernel/`, the RISC OS kernel; `filing/`, FileSwitch and its filing systems;
`basic/`, BBC BASIC V and VI; `vdu/`, the VDU drivers; `draw/`, Draw and
DrawFile; `font/`, the Font Manager and the ROM fonts; `clib/`,
SharedCLibrary; `session/`, the protocol between the front end and a
session's worker; and `app/`, the application itself. The design and its
staging are `plans/RISCOS-EMULATOR.md`; the description is
`docs/src/desktop/tairix-bridge.md`.

<!-- ANCHOR: notices -->
## Notices

RISC OS is a trademark of RISC OS Developments Ltd. TAIRiX Bridge uses the
name only to say which system's applications it runs.

TAIRiX Bridge is an independent project. It is not affiliated with, nor
endorsed, sponsored or approved by, RISC OS Developments Ltd, RISC OS Open
Ltd or any other holder of rights in RISC OS.

TAIRiX Bridge is a clean-room implementation. It contains no RISC OS code,
ROM image, font or message file. It is written from published documentation
and from what a running RISC OS machine is seen to do, never from RISC OS
source code, and what is recorded from such a machine to test against keeps
the values its calls answer, never its code or the text of its messages.

Other names used here may be trademarks of their respective owners.
<!-- ANCHOR_END: notices -->
