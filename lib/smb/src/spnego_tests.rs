//! Unit tests for SPNEGO, held to the tokens a Windows client and server
//! exchanged.

use alloc::vec::Vec;

use tairix_der::Writer;

use super::{initial, mech_list, offered, response, NegState, Reply, OID};
use crate::ntlm;
use crate::vectors::{bytes, qts_tokens, windows_ntlm};
use crate::SmbError;

/// NEGOEX's OID, 1.3.6.1.4.1.311.2.2.30, as DER contents.
const NEGOEX: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x1e];

#[test]
fn the_initial_token_frames_one_mechanism_as_windows_does() {
    let windows = windows_ntlm();
    assert_eq!(
        initial(ntlm::OID, &windows.negotiate).expect("encodes"),
        windows.init_buffer
    );
}

#[test]
fn a_later_token_reencodes_as_windows_sent_it() {
    let windows = windows_ntlm();
    assert_eq!(
        response(&windows.authenticate, &windows.client_mic).expect("encodes"),
        windows.answer_buffer
    );
}

#[test]
fn the_mechanism_list_is_the_one_the_mic_covers() {
    let list = mech_list(ntlm::OID).expect("encodes");
    assert_eq!(list, bytes("300c060a2b06010401823702020a"));
    let windows = windows_ntlm();
    assert!(windows
        .init_buffer
        .windows(list.len())
        .any(|window| window == list));
}

#[test]
fn windows_replies_decode() {
    let windows = windows_ntlm();
    let reply = Reply::decode(&windows.reply_buffer).expect("decodes");
    assert_eq!(reply.state, Some(NegState::AcceptIncomplete));
    assert_eq!(reply.mechanism, Some(ntlm::OID));
    assert_eq!(reply.token.map(<[u8]>::len), Some(148));
    assert_eq!(reply.mic, None);

    let last = Reply::decode(&windows.final_buffer).expect("decodes");
    assert_eq!(last.state, Some(NegState::AcceptCompleted));
    assert_eq!((last.mechanism, last.token), (None, None));
    assert_eq!(
        last.mic.map(<[u8]>::to_vec),
        Some(bytes("010000003932a87523ab660100000000"))
    );
}

#[test]
fn windows_hints_offer_negoex_and_ntlm() {
    let windows = windows_ntlm();
    assert_eq!(
        offered(&windows.hints).expect("decodes"),
        [NEGOEX, ntlm::OID]
    );
}

#[test]
fn sambas_hints_offer_ntlm_past_its_placeholder_hint() {
    let (hints, _) = qts_tokens();
    assert_eq!(offered(&hints).expect("decodes"), [ntlm::OID]);
}

/// `negTokenInit2` hints listing `mechanisms`, as `write` adds to them.
fn hints(mechanisms: &[&[u8]]) -> Vec<u8> {
    let mut w = Writer::new();
    w.constructed(tairix_der::Tag::application(0), |w| {
        w.oid(OID);
        w.explicit(0, |w| {
            w.sequence(|w| {
                w.explicit(0, |w| {
                    w.sequence(|w| {
                        for mechanism in mechanisms {
                            w.oid(mechanism);
                        }
                    });
                });
            });
        });
    });
    w.finish().expect("encodes")
}

#[test]
fn hints_are_bounded_and_belong_to_spnego() {
    let many: Vec<&[u8]> = core::iter::repeat_n(ntlm::OID, 17).collect();
    assert_eq!(offered(&hints(&many)), Err(SmbError::TooLarge));
    assert_eq!(offered(&hints(&many[..16])).map(|m| m.len()), Ok(16));
    assert_eq!(offered(&hints(&[])), Ok(Vec::new()));

    let mut foreign = hints(&[ntlm::OID]);
    let at = foreign
        .windows(OID.len())
        .position(|window| window == OID)
        .expect("SPNEGO's OID");
    foreign[at + OID.len() - 1] ^= 0x01;
    assert_eq!(offered(&foreign), Err(SmbError::Malformed));
    assert_eq!(offered(&[]), Err(SmbError::Malformed));
}

#[test]
fn a_malformed_or_foreign_reply_is_refused() {
    let windows = windows_ntlm();
    let mut trailing = windows.final_buffer.clone();
    trailing.push(0);
    let mut state = windows.final_buffer.clone();
    let at = state
        .windows(3)
        .position(|window| window == [0x0a, 0x01, 0x00])
        .expect("negState");
    state[at + 2] = 4;
    let mut outer = windows.final_buffer.clone();
    outer[0] = 0xa0;
    for (token, err) in [
        (trailing, SmbError::Malformed),
        (state, SmbError::Malformed),
        (outer, SmbError::Malformed),
        (windows.final_buffer[..10].to_vec(), SmbError::Malformed),
        (Vec::new(), SmbError::Malformed),
    ] {
        assert_eq!(Reply::decode(&token), Err(err), "{token:02x?}");
    }
}
