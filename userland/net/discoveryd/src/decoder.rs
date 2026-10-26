//! The decoder: the sandboxed worker that runs the multicast DNS and
//! WS-Discovery engines.
//!
//! It is the only process that ever parses a byte a peer sent, so it holds
//! nothing: the kernel's sandbox spawn mode brands it capability-empty and
//! confines it to its two pipes. Time and randomness therefore come from the
//! front — every frame that moves time carries `now`, and the engines' CSPRNG
//! is keyed by the front's draw — and the decoder reports the one instant its
//! engines next need time back.
//!
//! One engine per interface the front says is up, so a record learned on one
//! link can never answer for another, and a link that goes down takes
//! everything learned on it. Each spends the interface's group budget as the
//! front holds it, so a query the budget cannot pay for waits, still due,
//! rather than being counted sent and dropped. Every question the front asks is asked on every
//! such interface under the front's own id, and each answer crosses back
//! already typed — a browse's instance label, a resolve's target and port —
//! so the front reads a field rather than a record. A record that does not
//! read as its question's form (a browse `PTR` naming an instance of another
//! type) is not an answer to it.
//!
//! Each link also runs a WS-Discovery engine, browsing only while the front
//! asks the computers question. It names a computer by asking the front to
//! fetch its metadata — the front builds and sends the request, so this
//! process can steer a connection at nothing — and reads the response the
//! front relays back unread.

use alloc::vec::Vec;

use tairix_abi::discovery_ipc::{Answer, Change, Entry, Membership, ServiceTypeField};
use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_abi::time::Duration64;
use tairix_hash::HashSeed;
use tairix_net::dns::Name;
use tairix_net::dnssd::{ServiceInstance, ServiceType};
use tairix_net::http::HostName;
use tairix_net::mdns::{
    Answer as EngineAnswer, AnswerChange, Destination, MdnsEngine, QuestionId, RData, Record,
    Sender, MAX_MESSAGE_LEN, MAX_QUESTIONS,
};
use tairix_net::rate::TokenBucket;
use tairix_net::wsd::{self, Change as WsdChange, Computer, Output as WsdOutput, WsdEngine};
use tairix_net::IpAddr;
use tairix_rng::{FastRng, RandU64};
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_util::fallible;

use crate::wire::{
    Budgets, Edge, FetchRequest, Form, FromDecoder, FromWsd, ToDecoder, ToWsd, GROUP_BURST,
    GROUP_RATE, GROUP_RESERVE, MAX_FROM_DECODER, WSD_BURST, WSD_RATE,
};

/// The service a decoder worker serves over its pipes.
#[derive(Default)]
pub struct Decoder {
    configured: Option<Configured>,
    links: Vec<Link>,
    /// Every question the front asks, at its id.
    questions: Vec<Option<Question>>,
    /// The computers question's id, while the front asks it.
    computers: Option<u32>,
    /// The deadline the front was last told of. It starts as the front's own
    /// belief about a fresh decoder: nothing due.
    reported: Option<u64>,
    /// The latest instant seen, so time never runs backwards.
    now: u64,
}

/// What the front's configuration keyed, and the buffers it paid for.
struct Configured {
    cache: HashSeed,
    rng: FastRng,
    /// Where an engine builds a datagram.
    message: Vec<u8>,
    /// Where a frame to the front is built.
    frame: Vec<u8>,
}

/// One question the front asks.
#[derive(Clone, Copy)]
struct Question {
    id: u32,
    form: Form,
    name: Name,
}

/// One interface's engines.
struct Link {
    name: [u8; IF_NAME_LEN],
    engine: MdnsEngine,
    /// The interface's group budget, charged as the front charges it.
    group: TokenBucket,
    wsd: WsdEngine,
    /// The interface's WS-Discovery budget, charged as the front charges it.
    wsd_group: TokenBucket,
}

impl Link {
    /// When its engines next have work their budgets let them do.
    fn deadline(&self) -> Option<u64> {
        let mdns = self
            .engine
            .next_deadline()
            .and_then(|due| Some(due.max(self.group.ready_at(GROUP_RESERVE)?)));
        let wsd = self.wsd.next_deadline(self.wsd_group.ready_at(0));
        mdns.into_iter()
            .chain(wsd)
            .min()
            .map(|due| due.saturating_total_nanos())
    }
}

impl Decoder {
    /// A decoder awaiting its configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Report the engines' folded deadline when it changed, or always when
    /// `answering` a tick.
    fn report(&mut self, frame: &mut [u8], out: &mut dyn FrameOut, answering: bool) -> bool {
        let deadline = self.links.iter().filter_map(Link::deadline).min();
        if !answering && self.reported == deadline {
            return true;
        }
        self.reported = deadline;
        send(frame, out, &FromDecoder::Deadline(deadline))
    }
}

/// One CSPRNG word for an engine's jitter.
fn draw(rng: &mut FastRng) -> u32 {
    let mut word = [0u8; 4];
    rng.fill_bytes(&mut word);
    u32::from_le_bytes(word)
}

/// Encode `message` into `frame` and write it, reporting whether the front
/// is still listening.
fn send(frame: &mut [u8], out: &mut dyn FrameOut, message: &FromDecoder<'_>) -> bool {
    message
        .encode(frame)
        .is_some_and(|len| out.frame(&frame[..len]).is_ok())
}

impl SessionService for Decoder {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        // The front never sends a frame it did not mean, so one that does not
        // decode, or arrives out of order, ends the session rather than being
        // guessed at.
        let Ok(frame) = ToDecoder::decode(request) else {
            return SessionStep::Finished;
        };
        if let ToDecoder::Configure { cache_key, rng_key } = frame {
            if self.configured.is_some() {
                return SessionStep::Finished;
            }
            let (Some(message), Some(frame)) = (
                fallible::filled(MAX_MESSAGE_LEN, 0u8),
                fallible::filled(MAX_FROM_DECODER, 0u8),
            ) else {
                return SessionStep::Finished;
            };
            self.configured = Some(Configured {
                cache: HashSeed::from_bytes(*cache_key),
                rng: FastRng::from_key(rng_key),
                message,
                frame,
            });
            return SessionStep::Continue;
        }
        let Some(mut configured) = self.configured.take() else {
            return SessionStep::Finished;
        };
        let answering = matches!(frame, ToDecoder::Tick { .. });
        let alive = self.apply(&mut configured, frame, out)
            && self.transmit(&mut configured, out)
            && self.report(&mut configured.frame, out, answering);
        self.configured = Some(configured);
        if alive {
            SessionStep::Continue
        } else {
            SessionStep::Finished
        }
    }
}

impl Decoder {
    /// Carry out one frame, reporting whether the session goes on.
    fn apply(
        &mut self,
        configured: &mut Configured,
        frame: ToDecoder<'_>,
        out: &mut dyn FrameOut,
    ) -> bool {
        match frame {
            ToDecoder::Configure { .. } => false,
            ToDecoder::Datagram {
                now,
                interface,
                source,
                port,
                to_group,
                payload,
            } => {
                self.now = self.now.max(now);
                let sender = Sender {
                    addr: source,
                    port,
                    // The front relays only what the stack found on-link.
                    on_link: true,
                    to_group,
                };
                self.on_datagram(configured, interface, sender, payload, out)
            }
            // Time moves; the engines' due work is done below.
            ToDecoder::Tick { now } => {
                self.now = self.now.max(now);
                true
            }
            ToDecoder::Link {
                now,
                interface,
                edge,
            } => {
                self.now = self.now.max(now);
                let at = self.links.iter().position(|link| link.name == interface);
                let up = matches!(edge, Edge::Up(_));
                let carried = match (edge, at) {
                    (Edge::Up(budgets), None) => self.link_up(configured, interface, budgets, out),
                    (Edge::Down, Some(at)) => {
                        self.links.swap_remove(at);
                        true
                    }
                    // The front tells each link's edge once.
                    _ => false,
                };
                carried
                    && send(
                        &mut configured.frame,
                        out,
                        &FromDecoder::Linked { interface, up },
                    )
            }
            ToDecoder::Ask {
                now,
                question,
                form,
                name,
            } => {
                self.now = self.now.max(now);
                self.on_ask(configured, question, form, name, out)
            }
            ToDecoder::Stop { question } => {
                self.on_stop(question)
                    && send(
                        &mut configured.frame,
                        out,
                        &FromDecoder::Stopped { question },
                    )
            }
            ToDecoder::Replay { question, token } => {
                self.replay(&mut configured.frame, question, token, out)
            }
            ToDecoder::Wsd(frame) => self.on_wsd(configured, frame, out),
        }
    }

    /// Hand one WS-Discovery frame to its interface's engine, relaying the
    /// computers it learns or loses.
    fn on_wsd(
        &mut self,
        configured: &mut Configured,
        frame: ToWsd<'_>,
        out: &mut dyn FrameOut,
    ) -> bool {
        let (now, interface) = match frame {
            ToWsd::Datagram { now, interface, .. }
            | ToWsd::Fetched { now, interface, .. }
            | ToWsd::FetchFailed { now, interface, .. } => (now, interface),
        };
        self.now = self.now.max(now);
        let at = Duration64::from_nanos(self.now);
        let computers = self.computers;
        let Some(link) = self.links.iter_mut().find(|link| link.name == interface) else {
            // The front relays only to an engine it has told up.
            return !matches!(frame, ToWsd::Datagram { .. });
        };
        let frame_out = &mut configured.frame;
        let mut alive = true;
        let mut relay = |change: WsdChange| {
            alive &= relay_computer(frame_out, out, interface, computers, &change);
        };
        match frame {
            ToWsd::Datagram {
                source, payload, ..
            } => {
                link.wsd.on_datagram(at, payload, source, &mut relay);
            }
            ToWsd::Fetched {
                fetch, response, ..
            } => link.wsd.on_fetched(at, fetch, response, &mut relay),
            ToWsd::FetchFailed { fetch, .. } => link.wsd.on_fetch_failed(at, fetch),
        }
        alive
    }

    /// Hand one relayed datagram to its interface's engine, relaying what it
    /// learns and sending what it answers.
    fn on_datagram(
        &mut self,
        configured: &mut Configured,
        interface: [u8; IF_NAME_LEN],
        sender: Sender,
        payload: &[u8],
        out: &mut dyn FrameOut,
    ) -> bool {
        let Some(link) = self.links.iter_mut().find(|link| link.name == interface) else {
            return false;
        };
        let Configured {
            rng,
            message,
            frame,
            ..
        } = configured;
        let mut alive = true;
        let Link {
            name,
            engine,
            group,
            ..
        } = link;
        let emitted = engine.on_message(
            Duration64::from_nanos(self.now),
            payload,
            sender,
            &mut || draw(rng),
            message,
            &mut |answer| {
                alive &= relay(frame, out, *name, &self.questions, answer);
            },
        );
        // An answer sent at once is a defence of a name this host owns,
        // which the reserve is kept for; one past even that is not sent, so
        // a flood of probes cannot be turned into a multicast flood.
        let affordable = |emit: &tairix_net::mdns::Emit| {
            emit.to != Destination::Group || group.allow(Duration64::from_nanos(self.now))
        };
        if let Some(emit) = emitted.filter(affordable) {
            alive &= send(
                frame,
                out,
                &FromDecoder::Transmit {
                    interface,
                    at: self.now,
                    to: emit.to,
                    payload: &message[..emit.len],
                },
            );
        }
        alive
    }

    /// Take on one question and ask it on every link.
    fn on_ask(
        &mut self,
        configured: &mut Configured,
        question: u32,
        form: Form,
        name: &[u8],
        out: &mut dyn FrameOut,
    ) -> bool {
        let Some(name) = Name::from_wire(name) else {
            return false;
        };
        let slot = question as usize;
        // The front asks each id once until it is stopped, and none past the
        // bound; a frame that does either is not one it sends.
        if slot >= MAX_QUESTIONS {
            return false;
        }
        if self.questions.len() <= slot {
            if self
                .questions
                .try_reserve(slot + 1 - self.questions.len())
                .is_err()
            {
                return false;
            }
            self.questions.resize(slot + 1, None);
        }
        if self.questions[slot].is_some() || (form == Form::Computer && self.computers.is_some()) {
            return false;
        }
        if form == Form::Computer {
            self.computers = Some(question);
        }
        let asked = Question {
            id: question,
            form,
            name,
        };
        self.questions[slot] = Some(asked);
        (0..self.links.len()).all(|link| self.ask_on(configured, link, &asked, out))
    }

    /// Stop asking one question on every link.
    fn on_stop(&mut self, question: u32) -> bool {
        let Some(slot) = self
            .questions
            .get_mut(question as usize)
            .filter(|slot| slot.is_some())
        else {
            return false;
        };
        *slot = None;
        if self.computers == Some(question) {
            self.computers = None;
            for link in &mut self.links {
                link.wsd.stop();
            }
            return true;
        }
        for link in &mut self.links {
            link.engine.stop_asking(QuestionId::new(question));
        }
        true
    }

    /// Start engines for `interface`, spending `budgets`, and ask them every
    /// question.
    fn link_up(
        &mut self,
        configured: &mut Configured,
        interface: [u8; IF_NAME_LEN],
        budgets: Budgets,
        out: &mut dyn FrameOut,
    ) -> bool {
        if self.links.try_reserve(1).is_err() {
            return false;
        }
        self.links.push(Link {
            name: interface,
            engine: MdnsEngine::new(configured.cache),
            group: TokenBucket::with_level(GROUP_BURST, GROUP_RATE, budgets.group),
            wsd: WsdEngine::new(configured.cache),
            wsd_group: TokenBucket::with_level(WSD_BURST, WSD_RATE, budgets.wsd),
        });
        let link = self.links.len() - 1;
        (0..self.questions.len()).all(|slot| match self.questions[slot] {
            Some(asked) => self.ask_on(configured, link, &asked, out),
            None => true,
        })
    }

    /// Ask `asked` on link `link`, relaying what its cache already holds. An
    /// engine that refuses a question the front bounded is not one the
    /// session can go on with.
    fn ask_on(
        &mut self,
        configured: &mut Configured,
        link: usize,
        asked: &Question,
        out: &mut dyn FrameOut,
    ) -> bool {
        let Self {
            links,
            questions,
            now,
            ..
        } = self;
        let Some(link) = links.get_mut(link) else {
            return false;
        };
        let Configured { rng, frame, .. } = configured;
        let Some(record_type) = asked.form.record_type() else {
            link.wsd
                .browse(Duration64::from_nanos(*now), &mut || draw(rng));
            return true;
        };
        let mut alive = true;
        let name = link.name;
        let asked_on = link.engine.ask(
            Duration64::from_nanos(*now),
            QuestionId::new(asked.id),
            asked.name,
            record_type,
            &mut || draw(rng),
            &mut |answer| {
                alive &= relay(frame, out, name, questions, answer);
            },
        );
        asked_on.is_ok() && alive
    }

    /// Send what `question` holds on every link under `token`, then the
    /// marker that ends the replay.
    fn replay(
        &mut self,
        frame: &mut [u8],
        question: u32,
        token: u32,
        out: &mut dyn FrameOut,
    ) -> bool {
        if let Some(asked) = asked(&self.questions, question) {
            let Some(record_type) = asked.form.record_type() else {
                for link in &self.links {
                    for (key, addr, computer) in link.wsd.computers() {
                        let entry =
                            computer_entry(question, link.name, Change::Added, key, addr, computer);
                        if !send(frame, out, &FromDecoder::Held { token, entry }) {
                            return false;
                        }
                    }
                }
                return send(frame, out, &FromDecoder::Replayed { token });
            };
            for link in &self.links {
                let cache = link.engine.cache();
                let key = cache.key_of(&asked.name, record_type);
                for cached in cache.lookup(key, &asked.name, record_type) {
                    let Some(answer) = typed(&asked, &cached.record) else {
                        continue;
                    };
                    let entry = Entry::Answer {
                        request: question,
                        interface: link.name,
                        change: Change::Added,
                        ttl: cached.record.ttl,
                        answer,
                    };
                    if !send(frame, out, &FromDecoder::Held { token, entry }) {
                        return false;
                    }
                }
            }
        }
        send(frame, out, &FromDecoder::Replayed { token })
    }

    /// Run every engine's due work at the current instant — expire, refresh,
    /// and send each question that is due — while its interface's group
    /// budget, less the reserve, can pay for what that sends. Work it cannot
    /// pay for stays due until the budget can.
    fn transmit(&mut self, configured: &mut Configured, out: &mut dyn FrameOut) -> bool {
        let now = Duration64::from_nanos(self.now);
        let Configured { message, frame, .. } = configured;
        for link in &mut self.links {
            let Link {
                name,
                engine,
                group,
                ..
            } = link;
            let mut alive = true;
            while group.ready(now, GROUP_RESERVE) {
                let Some(emit) = engine.poll(now, message, &mut |answer| {
                    alive &= relay(frame, out, *name, &self.questions, answer);
                }) else {
                    break;
                };
                if emit.to == Destination::Group {
                    group.allow(now);
                }
                alive &= send(
                    frame,
                    out,
                    &FromDecoder::Transmit {
                        interface: *name,
                        at: self.now,
                        to: emit.to,
                        payload: &message[..emit.len],
                    },
                );
                if !alive {
                    return false;
                }
            }
            if !alive {
                return false;
            }
        }
        self.transmit_wsd(configured, out)
    }

    /// Run every WS-Discovery engine's due work at the current instant: send
    /// what its link's budget can pay for, and ask the front for each fetch
    /// it is due to make.
    fn transmit_wsd(&mut self, configured: &mut Configured, out: &mut dyn FrameOut) -> bool {
        let now = Duration64::from_nanos(self.now);
        let computers = self.computers;
        let Configured {
            rng,
            message,
            frame,
            ..
        } = configured;
        for link in &mut self.links {
            let Link {
                name,
                wsd,
                wsd_group,
                ..
            } = link;
            let mut alive = true;
            loop {
                let output = wsd.poll(
                    now,
                    &mut || draw(rng),
                    message,
                    &mut |change| {
                        alive &= relay_computer(frame, out, *name, computers, &change);
                    },
                    &mut || wsd_group.allow(now),
                );
                let sent = match output {
                    Some(WsdOutput::Transmit(emit)) => send(
                        frame,
                        out,
                        &FromDecoder::Wsd(FromWsd::Transmit {
                            interface: *name,
                            at: self.now,
                            payload: &message[..emit.len],
                        }),
                    ),
                    Some(WsdOutput::Fetch(fetch)) => send(
                        frame,
                        out,
                        &FromDecoder::Wsd(FromWsd::Fetch(FetchRequest {
                            interface: *name,
                            fetch: fetch.id,
                            addr: fetch.addr,
                            port: fetch.port,
                            path: fetch.path.as_str(),
                            endpoint: fetch.endpoint.as_str(),
                            name: fetch.name.map(HostName::as_str),
                        })),
                    ),
                    None => break,
                };
                if !(sent && alive) {
                    return false;
                }
            }
            if !alive {
                return false;
            }
        }
        true
    }
}

/// Relay one WS-Discovery change as the computers question's answer.
fn relay_computer(
    frame: &mut [u8],
    out: &mut dyn FrameOut,
    interface: [u8; IF_NAME_LEN],
    computers: Option<u32>,
    change: &WsdChange,
) -> bool {
    let Some(request) = computers else {
        return true;
    };
    let mut answer = |change, key, addr, computer: &Computer| {
        let entry = computer_entry(request, interface, change, key, addr, computer);
        send(frame, out, &FromDecoder::Answer(entry))
    };
    match change {
        WsdChange::Found {
            key,
            addr,
            computer,
        } => answer(Change::Added, *key, *addr, computer),
        WsdChange::Gone {
            key,
            addr,
            computer,
        } => answer(Change::Retired, *key, *addr, computer),
    }
}

/// `computer` as the computers question's answer on `interface`.
fn computer_entry(
    request: u32,
    interface: [u8; IF_NAME_LEN],
    change: Change,
    key: u64,
    address: IpAddr,
    computer: &Computer,
) -> Entry<'_> {
    Entry::Answer {
        request,
        interface,
        change,
        ttl: 0,
        answer: Answer::Computer {
            key,
            address,
            name: computer.name.as_str(),
            membership: match &computer.membership {
                wsd::Membership::Workgroup(group) => Membership::Workgroup(group.as_str()),
                wsd::Membership::Domain(domain) => Membership::Domain(domain.as_str()),
                wsd::Membership::Unstated => Membership::Unstated,
            },
        },
    }
}

/// The question the front asks under `id`.
fn asked(questions: &[Option<Question>], id: u32) -> Option<Question> {
    questions.get(id as usize).copied().flatten()
}

/// Relay one engine answer as the front's question it answers, which the
/// engine names by the front's own id.
fn relay(
    frame: &mut [u8],
    out: &mut dyn FrameOut,
    interface: [u8; IF_NAME_LEN],
    questions: &[Option<Question>],
    answer: &EngineAnswer<'_>,
) -> bool {
    let Some(question) = asked(questions, answer.question.get()) else {
        return true;
    };
    let Some(typed) = typed(&question, &answer.record.record) else {
        return true;
    };
    let change = match answer.change {
        AnswerChange::Added => Change::Added,
        AnswerChange::Refreshed => Change::Refreshed,
        AnswerChange::Retired => Change::Retired,
    };
    send(
        frame,
        out,
        &FromDecoder::Answer(Entry::Answer {
            request: question.id,
            interface,
            change,
            ttl: answer.record.record.ttl,
            answer: typed,
        }),
    )
}

/// What `record` says as an answer to `question`, or `None` when it does not
/// read as the question's form.
fn typed<'r>(question: &Question, record: &'r Record) -> Option<Answer<'r>> {
    Some(match (question.form, &record.data) {
        (Form::Instance, RData::Ptr(target)) => {
            let instance = ServiceInstance::from_name(target).ok()?;
            let parent = instance.service().to_name(instance.domain()).ok()?;
            if parent != question.name {
                return None;
            }
            Answer::Instance {
                label: target.labels().next()?,
            }
        }
        (Form::Service, RData::Srv(service)) => Answer::Service {
            priority: service.priority,
            weight: service.weight,
            port: service.port,
            target: service.target.as_wire(),
        },
        (Form::Text, RData::Txt(text)) => Answer::Text {
            octets: text.as_octets(),
        },
        (Form::AddressV4, RData::A(address)) => Answer::Address {
            address: IpAddr::V4(*address),
        },
        (Form::AddressV6, RData::Aaaa(address)) => Answer::Address {
            address: IpAddr::V6(*address),
        },
        (Form::Pointer, RData::Ptr(target)) => Answer::Pointer {
            target: target.as_wire(),
        },
        (Form::Type, RData::Ptr(target)) => {
            let (service, domain) = ServiceType::from_name(target).ok()?;
            if domain != Name::encode("local").ok()? {
                return None;
            }
            let label = target.labels().next()?;
            Answer::Type {
                service: ServiceTypeField {
                    name: label.get(1..)?,
                    transport: service.transport(),
                },
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
#[path = "decoder_tests.rs"]
mod tests;
