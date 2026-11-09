# `tairix-enrolment` — the service enrolment store

`lib/enrolment` decides which discovered services are *eligible* to start
(`plans/NEW-SERVICEMANAGER.md` SVC-3). Dropping a signed service bundle on
disk never makes a live service appear: eligibility is an explicit, recorded
decision, and a service not positively enrolled never starts. Activation is
the service manager's (`userland/system/init`); this crate owns only the
record.

## Two layers

- `Enrolment` — the image's own decision: the enrolment-governed services
  PID 1's startup configuration declares, built with `Enrolment::of`. It is
  compiled in, because nothing under `/System` is reliably readable at the
  instant the manager decides, so it is never parsed from a document.
- `EnrolmentOverride` — the administrator's document at
  `/System/Settings/Services/overrides` on the root volume (and a user's
  own under `Settings/Services/`), holding only the services whose enrolment
  was *changed* from the image's default, so an update shipping a new
  default applies at once to everything the administrator never touched.

`enabled(vendor, overrides, name)` is the one precedence — the
administrator's word where they have spoken about a service, else the
image's — and a query, so asking it allocates nothing.
`EnrolmentOverride::set(vendor, name, wanted)` makes one service's change in
place, holding an entry only where it differs from the image's, and returns
the `Change` that `EnrolmentOverride::undo` takes back without the heap, so a
change whose write is refused is restored rather than left recorded.

## Fail closed, and no authority

The override document is untrusted input, read by the `key value` grammar
every `/System/Settings` document shares (`tairix_util::conf`). A malformed
one — a bad name, a duplicate, a missing or unknown disposition, one past
`MAX_DOCUMENT_LEN` — is refused whole with `EnrolError`, carrying the line
that raised it, and a refused document is answered as a missing one is: the
image's layer stands. A document the heap cannot hold is refused as
`EnrolError::OutOfMemory`, which a reader tries again later rather than
answering as a missing one. `to_store_text` holds the writer to the same bound, so
no change can produce a document every reader would refuse. The manager
writes the document beside itself and renames it into place, so a crash never
leaves a torn one to be refused, and it does so before it starts or stops the
service the change is about.
`validate_service_name` is the one service-name rule: a lowercase bundle
identifier, so a traversal- or case-collision-shaped token can never be
enrolled, and a service not positively enrolled never starts.

A change to the record decides eligibility, never authority. The kernel derives a service's capabilities from its signed
bundle and its account's ceiling at spawn, whatever this record says.

Consumers: the service manager, and `lib/syntax`, which validates an
override document an editor holds with this same parser. The crate is
`no_std` + `alloc` and `forbid(unsafe_code)`. Stability tier: experimental
(`lib/enrolment/README.md`).
