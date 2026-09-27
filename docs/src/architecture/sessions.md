# Sessions

A session is a set of processes that end together. It is **anchored** at one
process, and when that process dies the kernel ends every member of the
session and of every session nested in it: each is killed with the
uncatchable `Kill` (exit status 137), so nothing a program started outlives
the program it belongs to, and nothing can be left running where no one can
reach it.

Every process is in exactly one session, fixed at admission; no process ever
changes session. The **root session** holds PID 1 and the drivers the kernel
loads itself. It is anchored at nothing and never ends.

## Placement at spawn

The `spawn` attach block's session selector (`SpawnAttach::session`,
`tairix_abi::SpawnSession`) decides where the child goes:

| Selector | The child's session |
|---|---|
| `Inherit` | the spawner's own |
| `New` | a new session anchored at the child, nested in the one anchored at the spawner |
| `Anchored` | the session anchored at the spawner |
| `Join(instance)` | the session of the live process `instance`, which must lie within the spawner's own |

The two forms that name the session anchored at the spawner found it, nested
in the spawner's own session, the first time either is used. So a spawner
contains everything it starts: an `Anchored` child ends with the spawner, and
a `New` child ends with the spawner *or* with itself, taking whatever it
started along.

Every choice lies inside the spawner's own session, so creating a session
needs no capability and no process can escape the containment it was started
under. A parser sandbox worker must `Inherit`. Sessions nest at most
`SESSION_DEPTH_MAX` (16) deep.

A refusal is decided before any of the child exists:

| Errno | Cause |
|---|---|
| `NotFound` | a join naming no live instance within the spawner's session |
| `Interrupted` | the destination, or a session enclosing it, is ending |
| `LimitExceeded` | the nesting bound |

A child admitted while its session begins ending is *born dead*: it never
runs, and its parent reaps it once, as killed.

## How the system uses them

| Spawner | Child | Selector |
|---|---|---|
| `init` | each service, each console's login | `New` |
| `login` | the login screen, a desktop, a text shell | `New` |
| `login` | an elevated program (run, launch, capture) | `Join(requester)` |
| the desktop | every application | `Anchored` |
| the terminal | each window's shell | `New` |

Which gives:

- **Force-killing the desktop** ends every application it started, every
  terminal's shell, and every job in them — including elevated programs, which
  join their requester's session.
- **Logging out** closes windows first: the desktop sends every top-level
  window `CloseRequested` and keeps serving for up to `SESSION_CLOSE_GRACE`
  (5 s), so an application can finish through the window server it still has,
  then exits; the kernel ends whatever remains. `login` waits up to
  `SESSION_END_GRACE` (10 s) for the sessions it told to end.
- **Closing a terminal window** ends its shell (`Terminate`), which ends the
  jobs started in it. Ending the terminal ends every window's shell.
- A text login's background jobs end at logout.
- Stopping a service ends everything it started.

## In the kernel

Membership lives in the capability table, under the lock that guards its
records (`kernel/sec/src/session.rs`). Each member is indexed under its own
session and every ancestor, in one ordered `(session, member)` set, so a
containment check is one lookup, a join or a departure walks a chain bounded
by the depth limit, and a session's members are one ordered range. A session
is freed when its range empties.

Admission places the child at its final step, together with its capability
record, after it is registered with its parent — so a kill cannot land on a
half-admitted child, and a placement that has become impossible is caught
under the same lock as the insert.

When an anchor's record is removed its session is marked ending, and once its
own teardown is done the kernel walks the session's members in bounded
batches, releasing the table between them, and kills each through a claim
checked against the instance it read. A session inside one that is already
ending leaves its members to the outer walk, so teardown nests one level deep.
Each member ended is audited as `SessionMemberEnded` (4038) with its task,
process, name and session.
