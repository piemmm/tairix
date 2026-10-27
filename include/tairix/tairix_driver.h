/*
* TAIRiX abi-v1 C development header.
*
* GENERATED FILE - DO NOT EDIT BY HAND.
*
* Driver-class ABI core: manifest, kinds, errors (AGENTS.md sec.8, sec.9).
*
* This is part of the C-language view of the TAIRiX kernel/user ABI.
* It is generated from the single source of truth in `lib/abi` by
* `cargo xtask c-header --write` and verified on every CI run by
* `cargo xtask c-header`. Edit `lib/abi` and regenerate; never edit
* this file directly (AGENTS.md sec.2.2, sec.9).
*/

#ifndef TAIRIX_DRIVER_H
#define TAIRIX_DRIVER_H

#include <stdint.h>
#include "tairix_hwtree.h"
#include "tairix_manifest.h"
#include "tairix_time.h"

/* Magic word identifying an abi-v1 driver manifest ("DRV1" little-endian). */
#define TAIRIX_DRIVER_MANIFEST_MAGIC 0x31565244u
/* Maximum number of capability identifiers a driver manifest may request. */
#define TAIRIX_DRIVER_MANIFEST_MAX_CAPABILITIES 64u
/* Maximum number of bind-table entries a driver manifest may declare. */
#define TAIRIX_DRIVER_MANIFEST_MAX_BIND_KEYS 16u
/* Length, in bytes, of the Ed25519 signer public key. */
#define TAIRIX_DRIVER_SIGNER_PUBKEY_LEN 32u
/* Length, in bytes, of the Ed25519 manifest signature. */
#define TAIRIX_DRIVER_SIGNATURE_LEN 64u
/* Packed little-endian wire size of a driver manifest, in bytes. */
#define TAIRIX_DRIVER_MANIFEST_WIRE_LEN 140u
/* Packed little-endian wire size of one bind-table entry, in bytes. */
#define TAIRIX_DRIVER_BIND_KEY_WIRE_LEN 80u

/* Magic word identifying an abi-v1 driver register reply ("DRR1" little-endian). */
#define TAIRIX_DRIVER_REGISTER_REPLY_MAGIC 0x31525244u
/* `status` value of a successful register reply; any other value is a
 * TAIRIX_DRIVER_ERROR_* code. */
#define TAIRIX_DRIVER_REGISTER_STATUS_OK ((int32_t)0)
/* Packed little-endian wire size of a driver register reply, in bytes. */
#define TAIRIX_DRIVER_REGISTER_REPLY_WIRE_LEN 24u

/* Driver exit statuses (int32_t): one vocabulary for every driver. */
#define TAIRIX_DRIVER_EXIT_DECLINED ((int32_t)0)
#define TAIRIX_DRIVER_EXIT_REMOVED ((int32_t)0)
#define TAIRIX_DRIVER_EXIT_NO_HOST ((int32_t)80)
#define TAIRIX_DRIVER_EXIT_NO_RESOURCES ((int32_t)81)
#define TAIRIX_DRIVER_EXIT_BRINGUP_FAILED ((int32_t)82)
#define TAIRIX_DRIVER_EXIT_NO_SERVICE ((int32_t)83)
#define TAIRIX_DRIVER_EXIT_SERVE_FAILED ((int32_t)84)
#define TAIRIX_DRIVER_EXIT_DEVICE_FAULT ((int32_t)85)

/* Driver execution domain (uint8_t); IN_KERNEL additionally needs CAP_DRV_KERNEL. */
#define TAIRIX_DRIVER_KIND_USER_SPACE ((uint8_t)0u)
#define TAIRIX_DRIVER_KIND_IN_KERNEL ((uint8_t)1u)

/* Payload sensitivity hint (uint8_t); SENSITIVE requires zero-on-free. */
#define TAIRIX_BUFFER_CLASS_NON_SENSITIVE ((uint8_t)0u)
#define TAIRIX_BUFFER_CLASS_SENSITIVE ((uint8_t)1u)

/* Sentinel "no driver handle"; a live handle travels as a uint64_t. */
#define TAIRIX_DRIVER_HANDLE_NONE ((uint64_t)0ull)

/* Stable driver-ABI error codes (int32_t), disjoint from TAIRIX_E_* errno. */
#define TAIRIX_DRIVER_ERROR_BUFFER_TOO_SMALL ((int32_t)1)
#define TAIRIX_DRIVER_ERROR_BAD_MAGIC ((int32_t)2)
#define TAIRIX_DRIVER_ERROR_ABI_VERSION_UNSUPPORTED ((int32_t)3)
#define TAIRIX_DRIVER_ERROR_LENGTH_OUT_OF_RANGE ((int32_t)4)
#define TAIRIX_DRIVER_ERROR_OUT_OF_RANGE ((int32_t)5)
#define TAIRIX_DRIVER_ERROR_PERMISSION_DENIED ((int32_t)6)
#define TAIRIX_DRIVER_ERROR_NOT_FOUND ((int32_t)7)
#define TAIRIX_DRIVER_ERROR_SIGNATURE_INVALID ((int32_t)8)
#define TAIRIX_DRIVER_ERROR_UNSUPPORTED ((int32_t)9)
#define TAIRIX_DRIVER_ERROR_DEVICE_FAULT ((int32_t)10)
#define TAIRIX_DRIVER_ERROR_BUSY ((int32_t)11)
#define TAIRIX_DRIVER_ERROR_NOT_IMPLEMENTED ((int32_t)12)
#define TAIRIX_DRIVER_ERROR_NO_SPACE ((int32_t)13)
#define TAIRIX_DRIVER_ERROR_SEAT_REVOKED ((int32_t)14)
#define TAIRIX_DRIVER_ERROR_ENDPOINT_STALLED ((int32_t)15)
#define TAIRIX_DRIVER_ERROR_MEDIUM_ERROR ((int32_t)16)
#define TAIRIX_DRIVER_ERROR_DEVICE_OFFLINE ((int32_t)17)
#define TAIRIX_DRIVER_ERROR_TOO_MANY_LINKS ((int32_t)18)
#define TAIRIX_DRIVER_ERROR_ALREADY_EXISTS ((int32_t)19)
#define TAIRIX_DRIVER_ERROR_DIRECTORY_NOT_EMPTY ((int32_t)20)
#define TAIRIX_DRIVER_ERROR_DIRECTORY_CYCLE ((int32_t)21)
#define TAIRIX_DRIVER_ERROR_OUT_OF_MEMORY ((int32_t)22)
#define TAIRIX_DRIVER_ERROR_NO_BANDWIDTH ((int32_t)23)
#define TAIRIX_DRIVER_ERROR_STALE ((int32_t)24)
#define TAIRIX_DRIVER_ERROR_READ_ONLY ((int32_t)25)
#define TAIRIX_DRIVER_ERROR_TIMED_OUT ((int32_t)26)
#define TAIRIX_DRIVER_ERROR_NOT_READY ((int32_t)27)
#define TAIRIX_DRIVER_ERROR_INTERRUPTED ((int32_t)28)

/* PCI vendor ID assigned to virtio devices (uint16_t; virtio 1.1 sec.4.1.2). */
#define TAIRIX_VIRTIO_PCI_VENDOR_ID ((uint16_t)0x1af4u)
/* virtio PCI capability `cfg_type` values (uint8_t; virtio 1.1 sec.4.1.4). */
#define TAIRIX_VIRTIO_PCI_CFG_COMMON ((uint8_t)1u)
#define TAIRIX_VIRTIO_PCI_CFG_NOTIFY ((uint8_t)2u)
#define TAIRIX_VIRTIO_PCI_CFG_ISR ((uint8_t)3u)
#define TAIRIX_VIRTIO_PCI_CFG_DEVICE ((uint8_t)4u)
#define TAIRIX_VIRTIO_PCI_CFG_PCI ((uint8_t)5u)

/* Mount-flag bitmap (uint32_t); any bit outside KNOWN_MASK is reserved and rejected. */
#define TAIRIX_MOUNT_FLAG_READ_ONLY ((uint32_t)0x1u)
#define TAIRIX_MOUNT_FLAG_NOEXEC ((uint32_t)0x2u)
#define TAIRIX_MOUNT_FLAG_KNOWN_MASK ((uint32_t)0x3u)

/* Sentinel "no node"; a live NodeId travels as a uint64_t. */
#define TAIRIX_NODE_ID_NONE ((uint64_t)0ull)

/* Display pixel encoding (uint8_t); named by the byte order of the first pixel. */
#define TAIRIX_DISPLAY_FORMAT_RGBA8888 ((uint8_t)1u)
#define TAIRIX_DISPLAY_FORMAT_BGRA8888 ((uint8_t)2u)

/* Display power state (uint8_t). */
#define TAIRIX_DISPLAY_POWER_ON ((uint8_t)1u)
#define TAIRIX_DISPLAY_POWER_OFF ((uint8_t)2u)

/* Filesystem node kind (uint8_t). */
#define TAIRIX_NODE_KIND_DIRECTORY ((uint8_t)0u)
#define TAIRIX_NODE_KIND_REGULAR_FILE ((uint8_t)1u)
#define TAIRIX_NODE_KIND_SYMLINK ((uint8_t)2u)

/* Why a format will not write a volume it opened (uint8_t); no code is 0. */
#define TAIRIX_WRITE_REFUSAL_FORMAT_READ_ONLY ((uint8_t)1u)
#define TAIRIX_WRITE_REFUSAL_UNSUPPORTED_FEATURE ((uint8_t)2u)
#define TAIRIX_WRITE_REFUSAL_ERRORS_RECORDED ((uint8_t)3u)
#define TAIRIX_WRITE_REFUSAL_HURD_LAYOUT ((uint8_t)4u)
#define TAIRIX_WRITE_REFUSAL_JOURNAL_UNUSABLE ((uint8_t)5u)
#define TAIRIX_WRITE_REFUSAL_NEEDS_CHECK ((uint8_t)6u)
#define TAIRIX_WRITE_REFUSAL_OPENED_READ_ONLY ((uint8_t)7u)
#define TAIRIX_WRITE_REFUSAL_COMMIT_FAILED ((uint8_t)8u)
#define TAIRIX_WRITE_REFUSAL_WRITE_PROTECTED ((uint8_t)9u)
#define TAIRIX_WRITE_REFUSAL_REVISION_UNWRITTEN ((uint8_t)10u)
#define TAIRIX_WRITE_REFUSAL_NOT_CLOSED ((uint8_t)11u)
#define TAIRIX_WRITE_REFUSAL_MEDIA_NOT_REWRITABLE ((uint8_t)12u)
#define TAIRIX_WRITE_REFUSAL_NO_SPACE_BITMAP ((uint8_t)13u)
#define TAIRIX_WRITE_REFUSAL_SPARABLE_MEDIA ((uint8_t)14u)
#define TAIRIX_WRITE_REFUSAL_WRITE_ONCE_MEDIA ((uint8_t)15u)
#define TAIRIX_WRITE_REFUSAL_METADATA_PARTITION ((uint8_t)16u)
#define TAIRIX_WRITE_REFUSAL_SEVERAL_PARTITIONS ((uint8_t)17u)
#define TAIRIX_WRITE_REFUSAL_DAMAGED ((uint8_t)18u)

/* Driver input-event kind (uint8_t); distinct from the windowing TAIRIX_INPUT_KIND_*. */
#define TAIRIX_INPUT_EVENT_KIND_KEY ((uint8_t)1u)
#define TAIRIX_INPUT_EVENT_KIND_POINTER ((uint8_t)2u)
#define TAIRIX_INPUT_EVENT_KIND_SCROLL ((uint8_t)3u)

/* Signed driver-manifest prefix; encoded little-endian on the wire. */
typedef struct tairix_driver_manifest {
    uint32_t magic;
    uint32_t abi_version;
    uint8_t kind;
    uint8_t bind_key_count;
    uint16_t capability_count;
    uint8_t syscall_table_hash[TAIRIX_SYSCALL_TABLE_HASH_LEN];
    uint8_t signer_pubkey[TAIRIX_DRIVER_SIGNER_PUBKEY_LEN];
    uint8_t signature[TAIRIX_DRIVER_SIGNATURE_LEN];
} tairix_driver_manifest_t;

/* One bind-table entry: a hardware-tree match key plus the manifest's
 * bind priority (AGENTS.md sec.18.3). bind_key_count entries follow the
 * capability body; all are covered by the manifest signature. */
typedef struct tairix_driver_bind_key {
    uint16_t priority;
    uint16_t reserved0;
    tairix_hw_match_key_t key;
} tairix_driver_bind_key_t;

/* Outcome of a spawned driver process's register() entry, sent to the
 * driver host over IPC; encoded little-endian on the wire. `status` is
 * TAIRIX_DRIVER_REGISTER_STATUS_OK or a TAIRIX_DRIVER_ERROR_* code; `handle` is
 * non-zero exactly when `status` is OK (informational only — the host
 * mints its own unforgeable handle). */
typedef struct tairix_driver_register_reply {
    uint32_t magic;
    uint32_t abi_version;
    int32_t status;
    uint32_t reserved0;
    uint64_t handle;
} tairix_driver_register_reply_t;

/* Block-device geometry (the drivers/storage class). */
typedef struct tairix_block_geometry {
    uint32_t block_size;
    uint64_t block_count;
} tairix_block_geometry_t;

/* Discard (TRIM/unmap) capability a block device reports. */
typedef struct tairix_discard_capability {
    uint8_t supported;
    uint64_t granularity_blocks;
    uint64_t max_blocks_per_request;
} tairix_discard_capability_t;

/* Point-in-time device-health snapshot (SMART / NVMe telemetry). */
typedef struct tairix_health_snapshot {
    uint64_t power_on_hours;
    uint64_t unsafe_shutdowns;
    uint64_t media_errors;
    uint64_t reallocated_sectors;
    uint64_t pending_sectors;
    uint64_t uncorrectable_sectors;
    uint64_t crc_errors;
    uint16_t percentage_used;
    uint16_t available_spare;
    uint16_t temperature_kelvin;
    uint8_t critical_warning;
} tairix_health_snapshot_t;

/* Identifying tuple for a discovered device (the drivers/bus class).
* `device_class` mirrors the Rust `class` field (renamed for C++). */
typedef struct tairix_bus_device {
    uint32_t vendor;
    uint32_t device;
    uint16_t device_class;
    uint16_t reserved0;
    uint64_t address;
} tairix_bus_device_t;

/* Active display mode (the drivers/display class); `format` is a
* TAIRIX_DISPLAY_FORMAT_*. */
typedef struct tairix_display_mode {
    uint32_t width_px;
    uint32_t height_px;
    uint32_t stride_bytes;
    uint8_t format;
} tairix_display_mode_t;

/* What a display's hardware layer compositor can do. */
typedef struct tairix_accel_caps {
    uint32_t max_layers;
    uint32_t max_width_px;
    uint32_t max_height_px;
    uint8_t per_layer_opacity;
    uint8_t hw_scaling;
    uint8_t band_free_blend;
} tairix_accel_caps_t;

/* The pointer image size a display's cursor plane shows. */
typedef struct tairix_cursor_caps {
    uint32_t max_width_px;
    uint32_t max_height_px;
} tairix_cursor_caps_t;

/* A display driver's name for a buffer it allocated; never zero. */
typedef struct tairix_buffer_id {
    uint32_t raw;
} tairix_buffer_id_t;

/* A rectangle of a display mode, in pixels from its top-left: what a
* present changed, or the rows the hosted console painted
* (console_damage). */
typedef struct tairix_damage_rect {
    uint32_t x;
    uint32_t y;
    uint32_t width_px;
    uint32_t height_px;
} tairix_damage_rect_t;

/* Where a display driver's device reaches the kernel console it hosts
* (console_adopt): the device address of its first pixel and its row
* stride, laid out as TAIRIX_DISPLAY_FORMAT_BGRA8888. reserved0 is zero. */
typedef struct tairix_hosted_console {
    uint64_t device_addr;
    uint32_t stride_bytes;
    uint32_t reserved0;
} tairix_hosted_console_t;

/* The four AGENTS.md sec.21 timestamps stored for a filesystem node. A
* stamp the backing format does not keep is the epoch (never a
* fabricated wall time). */
typedef struct tairix_node_times {
    tairix_time64_t created;
    tairix_time64_t modified;
    tairix_time64_t accessed;
    tairix_time64_t changed;
} tairix_node_times_t;

/* Structural metadata about a filesystem node; `kind` is a TAIRIX_NODE_KIND_*.
* `nlink` counts the directory entries naming the node; `ino` is its number
* as stat reports it; `times` carries its four timestamps, read in the same
* structural read as kind/size; and `content_gen` names the version of its
* data, 0 where the volume keeps none. */
typedef struct tairix_node_info {
    uint8_t kind;
    uint32_t nlink;
    uint64_t ino;
    uint64_t size;
    uint64_t allocated;
    tairix_node_times_t times;
    uint64_t content_gen;
} tairix_node_info_t;

/* One directory entry; `node` is a NodeId (uint64_t). The entry carries the
* child's full tairix_node_info_t (including its timestamps) and the opaque
* cursor that resumes the listing after it (pass it back to read_dir with
* the entry's name; 0 starts a listing). */
typedef struct tairix_dir_entry {
    uint64_t node;
    tairix_node_info_t info;
    uint64_t next_cursor;
} tairix_dir_entry_t;

/* A mounted volume's space accounting, in whole blocks of block_size bytes.
* avail_blocks <= free_blocks <= total_blocks always holds; files/files_free
* are 0 when the format tracks no fixed inode table. in_reserve is 1 when
* the volume is down to its reserve, so an ordinary write may be refused. */
typedef struct tairix_volume_stats {
    uint32_t block_size;
    uint8_t in_reserve;
    uint8_t reserved0[3];
    uint64_t total_blocks;
    uint64_t free_blocks;
    uint64_t avail_blocks;
    uint64_t files;
    uint64_t files_free;
} tairix_volume_stats_t;

/* A single input event; `kind` is a TAIRIX_INPUT_EVENT_KIND_*. */
typedef struct tairix_input_event {
    uint8_t kind;
    uint8_t reserved0;
    uint16_t code;
    int32_t value;
} tairix_input_event_t;

/* A 48-bit IEEE 802 link-layer address (the drivers/network class). */
typedef struct tairix_mac_address {
    uint8_t octets[TAIRIX_MAC_ADDRESS_LEN];
} tairix_mac_address_t;

#endif /* TAIRIX_DRIVER_H */
