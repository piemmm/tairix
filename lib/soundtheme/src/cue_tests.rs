use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::reply::encode_status_reply;
use tairix_abi::sound_cue::{encode_cue_reply, CueRequest, CUE_REPLY_LEN};
use tairix_abi::Errno;

use super::{
    cue, CuePost, CueTransport, PostedCues, CUE_ANSWER_DEADLINE_NS, MAX_POSTED, PLAYED_OUT_WAIT_NS,
    POLICY_ANSWER_DEADLINE_NS,
};
use crate::{Answer, CueOutcome, Silence, SoundEvent};

/// What a request asked, owned past the frame it was decoded from.
#[derive(Debug, Eq, PartialEq)]
enum Asked {
    Cue { event: SoundEvent, answer: Answer },
    Policy { document: String, files: [u64; 13] },
}

fn asked(request: &[u8]) -> Result<Asked, Errno> {
    Ok(match CueRequest::from_bytes(request)? {
        CueRequest::Cue { event, answer } => Asked::Cue { event, answer },
        CueRequest::Policy { document, files } => Asked::Policy {
            document: String::from(document),
            files,
        },
    })
}

/// An authority that answers every cue with `answer`, recording what it was
/// asked.
struct Authority {
    answer: Result<CueOutcome, Errno>,
    asked: Vec<Asked>,
}

impl Authority {
    fn answering(answer: Result<CueOutcome, Errno>) -> Self {
        Self {
            answer,
            asked: Vec::new(),
        }
    }
}

impl CueTransport for Authority {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let request = asked(request)?;
        let frame = encode_cue_reply(self.answer);
        self.asked.push(request);
        reply
            .get_mut(..frame.len())
            .ok_or(Errno::BufferTooSmall)?
            .copy_from_slice(&frame);
        Ok(frame.len())
    }
}

#[test]
fn a_cue_asks_for_its_event_and_reads_the_answer() {
    for answer in [
        Ok(CueOutcome::Played),
        Ok(CueOutcome::Silent(Silence::Disabled)),
        Err(Errno::PermissionDenied),
    ] {
        for when in [Answer::Started, Answer::PlayedOut] {
            let mut authority = Authority::answering(answer);
            assert_eq!(cue(&mut authority, SoundEvent::Login, when), answer);
            assert_eq!(
                authority.asked,
                [Asked::Cue {
                    event: SoundEvent::Login,
                    answer: when,
                }]
            );
        }
    }
}

/// A transport that fails, or answers with a frame of the wrong length.
struct Broken(Result<usize, Errno>);

impl CueTransport for Broken {
    fn call(&mut self, _request: &[u8], _reply: &mut [u8]) -> Result<usize, Errno> {
        self.0
    }
}

#[test]
fn a_broken_round_trip_is_its_failure_never_a_sound() {
    let bell = |transport: &mut Broken| cue(transport, SoundEvent::Bell, Answer::Started);
    assert_eq!(
        bell(&mut Broken(Err(Errno::NotFound))),
        Err(Errno::NotFound)
    );
    assert_eq!(
        bell(&mut Broken(Ok(CUE_REPLY_LEN + 1))),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(bell(&mut Broken(Ok(2))), Err(Errno::BufferTooSmall));
}

/// An authority answering posted cues and pushes when told to, recording what
/// it was asked, what was given up on and every change to the caller's wake.
#[derive(Default)]
struct Posts {
    asked: Vec<(u64, Asked, u64)>,
    answers: Vec<(u64, Result<CueOutcome, Errno>)>,
    statuses: Vec<(u64, Result<(), Errno>)>,
    cancelled: Vec<u64>,
    refuse: Option<Errno>,
    refuse_watch: Option<Errno>,
    watches: Vec<bool>,
}

impl CuePost for Posts {
    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        if let Some(err) = self.refuse {
            return Err(err);
        }
        let ticket = 100 + self.asked.len() as u64;
        self.asked.push((ticket, asked(request)?, deadline_ns));
        Ok(ticket)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        if let Some(at) = self.statuses.iter().position(|(held, _)| *held == ticket) {
            let frame = encode_status_reply(self.statuses.remove(at).1);
            reply[..frame.len()].copy_from_slice(&frame);
            return Ok(Some(frame.len()));
        }
        let Some(at) = self.answers.iter().position(|(held, _)| *held == ticket) else {
            return Ok(None);
        };
        let (_, answer) = self.answers.remove(at);
        let frame = encode_cue_reply(answer);
        reply[..frame.len()].copy_from_slice(&frame);
        Ok(Some(frame.len()))
    }

    fn cancel(&mut self, ticket: u64) {
        self.cancelled.push(ticket);
    }

    fn watch(&mut self, armed: bool) -> Result<(), Errno> {
        self.watches.push(armed);
        self.refuse_watch.map_or(Ok(()), Err)
    }
}

#[test]
fn a_posted_cue_is_answered_when_its_answer_arrives_and_never_waited_for() {
    let mut posts = Posts::default();
    let mut cues = PostedCues::new();
    assert!(!cues.waiting());
    cues.post(
        &mut posts,
        SoundEvent::DeviceAttached,
        Answer::Started,
        CUE_ANSWER_DEADLINE_NS,
    )
    .expect("posted");
    cues.post(
        &mut posts,
        SoundEvent::Shutdown,
        Answer::PlayedOut,
        PLAYED_OUT_WAIT_NS,
    )
    .expect("posted");
    assert!(cues.waiting());
    assert_eq!(
        posts.asked,
        [
            (
                100,
                Asked::Cue {
                    event: SoundEvent::DeviceAttached,
                    answer: Answer::Started
                },
                CUE_ANSWER_DEADLINE_NS
            ),
            (
                101,
                Asked::Cue {
                    event: SoundEvent::Shutdown,
                    answer: Answer::PlayedOut
                },
                PLAYED_OUT_WAIT_NS
            ),
        ]
    );
    assert_eq!(
        posts.watches,
        [true],
        "woken for answers once, while any is out"
    );
    let mut heard = Vec::new();
    cues.collect(&mut posts, |event, answer| heard.push((event, answer)));
    assert!(heard.is_empty(), "nothing has been answered");
    posts
        .answers
        .push((101, Ok(CueOutcome::Silent(Silence::Disabled))));
    cues.collect(&mut posts, |event, answer| heard.push((event, answer)));
    assert_eq!(
        heard,
        [(
            SoundEvent::Shutdown,
            Ok(CueOutcome::Silent(Silence::Disabled))
        )]
    );
    assert!(cues.waiting(), "the other is still out");
    assert_eq!(posts.watches, [true], "still woken for the other");
    cues.abandon(&mut posts);
    assert_eq!(posts.cancelled, [100]);
    assert!(!cues.waiting());
    assert_eq!(posts.watches, [true, false], "nothing left to wake for");
}

#[test]
fn the_wake_is_withdrawn_once_the_last_answer_is_collected() {
    let mut posts = Posts::default();
    let mut cues = PostedCues::new();
    cues.post(
        &mut posts,
        SoundEvent::Login,
        Answer::Started,
        CUE_ANSWER_DEADLINE_NS,
    )
    .expect("posted");
    posts.answers.push((100, Ok(CueOutcome::Played)));
    cues.collect(&mut posts, |_, _| {});
    assert!(!cues.waiting());
    assert_eq!(posts.watches, [true, false]);
    // A later cue arms it again rather than trusting a withdrawn wake.
    cues.post(
        &mut posts,
        SoundEvent::Logout,
        Answer::Started,
        CUE_ANSWER_DEADLINE_NS,
    )
    .expect("posted");
    assert_eq!(posts.watches, [true, false, true]);
}

#[test]
fn a_cue_that_cannot_wake_its_caller_is_given_up_on_at_once() {
    let mut posts = Posts {
        refuse_watch: Some(Errno::NotFound),
        ..Posts::default()
    };
    let mut cues = PostedCues::new();
    assert_eq!(
        cues.post(
            &mut posts,
            SoundEvent::Bell,
            Answer::Started,
            CUE_ANSWER_DEADLINE_NS
        ),
        Err(Errno::NotFound)
    );
    assert_eq!(
        posts.cancelled,
        [100],
        "an unwatched answer is never reaped"
    );
    assert!(!cues.waiting());
}

#[test]
fn a_caller_with_every_slot_out_is_refused_and_a_refused_post_holds_none() {
    let mut posts = Posts::default();
    let mut cues = PostedCues::new();
    let bell = |cues: &mut PostedCues, posts: &mut Posts| {
        cues.post(
            posts,
            SoundEvent::Bell,
            Answer::Started,
            CUE_ANSWER_DEADLINE_NS,
        )
    };
    for _ in 0..MAX_POSTED {
        bell(&mut cues, &mut posts).expect("posted");
    }
    assert_eq!(bell(&mut cues, &mut posts), Err(Errno::LimitExceeded));
    cues.abandon(&mut posts);
    posts.refuse = Some(Errno::NotFound);
    assert_eq!(bell(&mut cues, &mut posts), Err(Errno::NotFound));
    assert!(!cues.waiting(), "a refused post leaves nothing to wait for");
}

/// A push is posted like a cue — its answer collected, never waited for —
/// one at a time, and its status is what it comes to.
#[test]
fn a_posted_push_carries_its_document_and_files_and_reads_the_status() {
    let mut files = [0u64; 13];
    files[SoundEvent::Error.index()] = 9;
    for status in [Ok(()), Err(Errno::SeatNotOwner), Err(Errno::OutOfRange)] {
        let mut posts = Posts::default();
        let mut cues = PostedCues::new();
        cues.push(
            &mut posts,
            "sound.enabled = false\n",
            files,
            POLICY_ANSWER_DEADLINE_NS,
        )
        .expect("posted");
        assert_eq!(
            posts.asked,
            [(
                100,
                Asked::Policy {
                    document: String::from("sound.enabled = false\n"),
                    files,
                },
                POLICY_ANSWER_DEADLINE_NS
            )]
        );
        assert_eq!(
            cues.push(&mut posts, "", [0; 13], POLICY_ANSWER_DEADLINE_NS),
            Err(Errno::Busy),
            "one push at a time"
        );
        assert_eq!(
            cues.collect(&mut posts, |_, _| {}),
            None,
            "not yet answered"
        );
        assert!(cues.waiting());
        posts.statuses.push((100, status));
        assert_eq!(cues.collect(&mut posts, |_, _| {}), Some(status));
        assert!(!cues.waiting());
        assert_eq!(posts.watches, [true, false]);
    }
}

/// A push and a cue share the caller's one wake, and giving up gives up both.
#[test]
fn a_push_and_a_cue_share_one_wake_and_are_given_up_together() {
    let mut posts = Posts::default();
    let mut cues = PostedCues::new();
    cues.post(
        &mut posts,
        SoundEvent::Bell,
        Answer::Started,
        CUE_ANSWER_DEADLINE_NS,
    )
    .expect("posted");
    cues.push(&mut posts, "", [0; 13], POLICY_ANSWER_DEADLINE_NS)
        .expect("posted");
    assert_eq!(posts.watches, [true]);
    posts.answers.push((100, Ok(CueOutcome::Played)));
    assert_eq!(cues.collect(&mut posts, |_, _| {}), None);
    assert!(cues.waiting(), "the push is still out");
    assert_eq!(posts.watches, [true]);
    cues.abandon(&mut posts);
    assert_eq!(posts.cancelled, [101]);
    assert_eq!(posts.watches, [true, false]);
}

#[test]
fn a_push_no_request_can_carry_is_never_posted() {
    let mut posts = Posts::default();
    let mut cues = PostedCues::new();
    assert_eq!(
        cues.push(
            &mut posts,
            "sound.theme = a\tb\n",
            [0; 13],
            POLICY_ANSWER_DEADLINE_NS
        ),
        Err(Errno::OutOfRange)
    );
    assert!(posts.asked.is_empty());
    assert!(!cues.waiting());
}
