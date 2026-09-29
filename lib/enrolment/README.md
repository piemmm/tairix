# tairix-enrolment

Stability tier: **experimental**.

The service enrolment store engine: which discovered services are eligible
to start. Two layers — the image's own `Enrolment` and the administrator's
`EnrolmentOverride` document at `/System/Settings/Services/overrides` (and a
user's own under `Settings/Services/`) — folded by the one `effective`
precedence. Both parsers fail closed and name the line of a refusal; the
service-name rule (`validate_service_name`) is the one both layers and the
service manager's specifications use.

Consumers: the service manager (`userland/system/init`), and the text
editor's validation of the override document (`lib/syntax`).
