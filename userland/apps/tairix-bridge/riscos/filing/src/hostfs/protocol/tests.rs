//! The relay's messages: each crosses whole, nothing malformed or out of
//! bounds is read, and a reply is believed only where it answers its request.

use alloc::vec;
use alloc::vec::Vec;

use super::{Kind, Object, Refusal, Reply, Request, Version, DATA_MOST, ENTRIES_MOST, PATH_MOST};

fn encoded(request: &Request<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    request.encode(&mut out);
    out
}

fn answered(reply: &Reply<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    reply.encode(&mut out);
    out
}

const FILE: Object = Object {
    kind: Kind::File,
    load: 0xFFFF_FD45,
    exec: 0x6789_0ABC,
    length: 1000,
    attributes: 0x33,
};

const VERSION: Version = Version {
    volume: [0x5A; 16],
    node: 77,
    generation: 3,
};

#[test]
fn every_request_crosses_whole() {
    let requests = [
        Request::Roots,
        Request::Read {
            root: 1,
            path: b"Apps.!Edit",
        },
        Request::List {
            root: 0,
            path: b"",
            offset: 3,
            count: 8,
        },
        Request::Open {
            root: 2,
            path: b"Read/Me",
            update: true,
        },
        Request::ReadBytes {
            handle: 7,
            offset: 512,
            length: 256,
        },
        Request::WriteBytes {
            handle: 7,
            offset: 0,
            bytes: b"Hello",
        },
        Request::SetExtent {
            handle: 7,
            extent: 99,
        },
        Request::Sync { handle: 7 },
        Request::Close {
            handle: 7,
            stamp: Some((1, 2)),
        },
        Request::Close {
            handle: 7,
            stamp: None,
        },
        Request::Create {
            root: 0,
            path: b"New",
            load: 3,
            exec: 4,
            length: 5,
        },
        Request::CreateDir {
            root: 0,
            path: b"Dir",
            load: 3,
            exec: 4,
        },
        Request::WriteInfo {
            root: 0,
            path: b"File",
            load: 3,
            exec: 4,
            attributes: 0x13,
        },
        Request::Delete {
            root: 0,
            path: b"Gone",
        },
        Request::Rename {
            root: 0,
            from: b"A",
            to: b"B",
        },
        Request::Space { root: 3 },
    ];
    for request in requests {
        let bytes = encoded(&request);
        assert_eq!(Request::decode(&bytes), Some(request), "{request:?}");
        assert_eq!(
            Request::decode(&bytes[..bytes.len() - 1]),
            None,
            "cut short: {request:?}"
        );
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(Request::decode(&longer), None, "run on: {request:?}");
    }
    assert_eq!(Request::decode(&[99]), None, "an unknown request");
    assert_eq!(Request::decode(&[]), None);
}

#[test]
fn a_request_past_its_bounds_is_refused() {
    let long = vec![b'a'; PATH_MOST + 1];
    assert_eq!(
        Request::decode(&encoded(&Request::Read {
            root: 0,
            path: &long
        })),
        None
    );
    let big = vec![0; DATA_MOST as usize + 1];
    assert_eq!(
        Request::decode(&encoded(&Request::WriteBytes {
            handle: 1,
            offset: 0,
            bytes: &big
        })),
        None
    );
    for length in [DATA_MOST + 1, u32::MAX] {
        assert_eq!(
            Request::decode(&encoded(&Request::ReadBytes {
                handle: 1,
                offset: 0,
                length
            })),
            None
        );
    }
    let most = Request::ReadBytes {
        handle: 1,
        offset: 0,
        length: DATA_MOST,
    };
    assert_eq!(Request::decode(&encoded(&most)), Some(most));
    for count in [ENTRIES_MOST + 1, u32::MAX] {
        assert_eq!(
            Request::decode(&encoded(&Request::List {
                root: 0,
                path: b"",
                offset: 0,
                count
            })),
            None
        );
    }
    let most = Request::List {
        root: 0,
        path: b"",
        offset: 0,
        count: ENTRIES_MOST,
    };
    assert_eq!(Request::decode(&encoded(&most)), Some(most));
    let mut flag = encoded(&Request::Open {
        root: 0,
        path: b"F",
        update: false,
    });
    *flag.last_mut().expect("its flag") = 2;
    assert_eq!(Request::decode(&flag), None, "a flag is a flag");
}

/// A request naming `F` on the first root.
const READ_F: Request<'static> = Request::Read {
    root: 0,
    path: b"F",
};

/// Each kind of reply beside a request it answers.
fn answered_pairs() -> Vec<(Request<'static>, Reply<'static>)> {
    let open = |update| Request::Open {
        root: 0,
        path: b"F",
        update,
    };
    let opened = |writable, version| Reply::Opened {
        handle: 9,
        extent: 10,
        writable,
        version,
    };
    let list = Request::List {
        root: 0,
        path: b"",
        offset: 0,
        count: 2,
    };
    let read = Request::ReadBytes {
        handle: 1,
        offset: 0,
        length: 4,
    };
    let write = Request::WriteBytes {
        handle: 1,
        offset: 0,
        bytes: b"abcd",
    };
    let roots = Reply::Roots {
        seed: [7; 16],
        own: true,
        discs: vec![b"RISC\xA0OS".to_vec()],
    };
    let entries = Reply::Entries {
        next: u32::MAX,
        entries: vec![(b"Read/Me".to_vec(), FILE)],
    };
    let space = Reply::Space {
        free: 1 << 40,
        total: 1 << 41,
    };
    let delete = Request::Delete {
        root: 0,
        path: b"F",
    };
    vec![
        (Request::Roots, roots),
        (READ_F, Reply::Object(FILE)),
        (READ_F, Reply::Object(Object::NOTHING)),
        (list, entries),
        (open(false), opened(false, None)),
        (open(true), opened(true, Some(VERSION))),
        (read, Reply::Bytes(b"abcd")),
        (write, Reply::Wrote(2)),
        (Request::Sync { handle: 1 }, Reply::Done),
        (Request::Space { root: 0 }, space),
        (delete, Reply::Refused(Refusal::Locked)),
    ]
}

#[test]
fn a_reply_crosses_whole_where_it_answers_its_request() {
    for (request, reply) in answered_pairs() {
        let bytes = answered(&reply);
        assert_eq!(
            Reply::decode(&bytes, &request),
            Some(reply.clone()),
            "{reply:?}"
        );
        assert_eq!(
            Reply::decode(&bytes[..bytes.len() - 1], &request),
            None,
            "cut short: {reply:?}"
        );
    }
}

#[test]
fn a_reply_more_than_its_request_could_produce_is_not_believed() {
    let read = Request::ReadBytes {
        handle: 1,
        offset: 0,
        length: 2,
    };
    assert_eq!(
        Reply::decode(&answered(&Reply::Bytes(b"abc")), &read),
        None,
        "more bytes than asked"
    );
    let write = Request::WriteBytes {
        handle: 1,
        offset: 0,
        bytes: b"ab",
    };
    assert_eq!(
        Reply::decode(&answered(&Reply::Wrote(3)), &write),
        None,
        "more written than given"
    );
    let list = Request::List {
        root: 0,
        path: b"",
        offset: 0,
        count: 1,
    };
    let two = Reply::Entries {
        next: 2,
        entries: vec![(b"A".to_vec(), FILE), (b"B".to_vec(), FILE)],
    };
    assert_eq!(
        Reply::decode(&answered(&two), &list),
        None,
        "more entries than asked"
    );
    let bad = Reply::Entries {
        next: 1,
        entries: vec![(b"a b".to_vec(), FILE)],
    };
    assert_eq!(
        Reply::decode(&answered(&bad), &list),
        None,
        "a name RISC OS could not hold"
    );
}

#[test]
fn a_reply_of_a_kind_or_state_its_request_cannot_have_is_not_believed() {
    assert_eq!(
        Reply::decode(
            &answered(&Reply::Done),
            &Request::Read {
                root: 0,
                path: b"F"
            }
        ),
        None,
        "not what a read answers"
    );
    assert_eq!(
        Reply::decode(&answered(&Reply::Refused(Refusal::Failed)), &Request::Roots),
        None,
        "the roots are never refused"
    );
    let ownerless = Reply::Roots {
        seed: [7; 16],
        own: true,
        discs: Vec::new(),
    };
    assert_eq!(
        Reply::decode(&answered(&ownerless), &Request::Roots),
        None,
        "an own disc among no discs"
    );
    assert_eq!(
        Reply::decode(&[8, 200], &Request::Sync { handle: 1 }),
        None,
        "an unknown refusal"
    );
    let read_only = Request::Open {
        root: 0,
        path: b"F",
        update: false,
    };
    let writable = Reply::Opened {
        handle: 9,
        extent: 10,
        writable: true,
        version: None,
    };
    assert_eq!(
        Reply::decode(&answered(&writable), &read_only),
        None,
        "writable where only reading was asked"
    );
    let unversioned = Reply::Opened {
        handle: 9,
        extent: 10,
        writable: false,
        version: Some(Version {
            generation: 0,
            ..VERSION
        }),
    };
    assert_eq!(
        Reply::decode(&answered(&unversioned), &read_only),
        None,
        "a version with no generation"
    );
}
