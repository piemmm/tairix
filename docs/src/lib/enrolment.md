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
  `/System/Settings/Services/overrides` on the encrypted root (and a user's
  own under `Settings/Services/`), holding only the services whose enrolment
  was *changed* from the image's default, so an update shipping a new
  default applies at once to everything the administrator never touched.

`effective(vendor, overrides)` folds the pair, so no consumer re-derives the
precedence, and `overrides_for(vendor, desired)` writes back only what
differs.

## Fail closed, and no authority

The override document is untrusted input, read by the `key value` grammar
every `/System/Settings` document shares (`tairix_util::conf`). A malformed
one — a bad name, a duplicate, a missing or unknown disposition, one past
`MAX_DOCUMENT_LEN` — is refused whole with `EnrolError`, carrying the line
that raised it, and a refused document is answered as a missing one is: the
image's layer stands. The manager writes the document beside itself and
renames it into place, so a crash never leaves a torn one to be refused.
`validate_service_name` is the one service-name rule: a lowercase bundle
identifier, so a traversal- or case-collision-shaped token can never be
enrolled, and a service not positively enrolled never starts.

`enrol` and `unenrol` are pure record transforms: they decide eligibility,
never authority. The kernel derives a service's capabilities from its signed
bundle and its account's ceiling at spawn, whatever this record says.

Consumers: the service manager, and `lib/syntax`, which validates an
override document an editor holds with this same parser. The crate is
`no_std` + `alloc` and `forbid(unsafe_code)`. Stability tier: experimental
(`lib/enrolment/README.md`).
