# KERBEROS.md — The Kerberos 5 client

Staged plan for TAIRiX's Kerberos 5 client (RFC 4120) and its GSS-API
mechanism (RFC 4121), first consumed by the SMB client (`plans/SMB.md`) to
authenticate against Active Directory and MIT realms. **Binding under
`AGENTS.md`** (§2.12, §5, §19.5, §24). It consumes `plans/DNS.md` (KDC
location), `lib/crypto` (every primitive) and `lib/der` (`plans/SMB.md`
SMB3).

## Ledger

| id | What it is | Status |
|---|---|---|
| K1 | `lib/resolver` SRV lookups: KDC location through `_kerberos._tcp` / `_kerberos._udp`, ordered by priority and weight (RFC 2782) | planned |
| K2 | `lib/krb5` messages and crypto: the RFC 4120 ASN.1 types over `lib/der`, the RFC 3961 profile for AES-CTS-HMAC-SHA1-96 (17, 18) and AES-SHA2 (19, 20), string-to-key, n-fold, key derivation; published vectors | planned |
| K3 | `lib/krb5` exchanges: AS with PA-ENC-TIMESTAMP and ETYPE-INFO2, TGS with canonicalisation and cross-realm referrals, AP-REQ/AP-REP with subkeys, clock-skew correction, KDC transport (UDP, TCP on `KRB_ERR_RESPONSE_TOO_BIG`) | planned |
| K4 | The GSS-API Kerberos mechanism (RFC 4121): the initial context token, the authenticator checksum and its flags, AP-REP verification, the session key a consumer derives from, and the mechanism wired into `lib/smb`'s client beside NTLM | planned |

## 0. Decisions (binding)

- **Pure engine.** `lib/krb5` is `no_std` + `alloc`, `#![forbid(unsafe_code)]`,
  with no I/O, clock or randomness: the host supplies KDC replies, the time
  and CSPRNG output, and sends what the engine returns. It runs inside its
  consumer's sandbox worker, since a KDC reply is untrusted input (§19.5).
- **Enctypes.** 18, 17, 20 and 19 only. RC4-HMAC (23), DES and 3DES are
  refused: a KDC or ticket offering only those is an error naming the reason,
  never a fallback.
- **Pre-authentication** is PA-ENC-TIMESTAMP with the salt and iteration
  count ETYPE-INFO2 supplies, bounded. PKINIT and FAST are absent; a KDC that
  demands FAST is refused with the reason.
- **Principals.** Kerberos is used for a principal written `user@REALM`;
  `DOMAIN\user` names no realm, so it authenticates with NTLMv2 instead
  (SMB.md §0). The service principal is `cifs/<host>@REALM` from the DNS name
  the user chose; a host chosen by address cannot use Kerberos.
- **KDC location** is DNS SRV, or an explicit `kdc=` the caller supplies.
  There is no static realm file to drift from the DNS.
- **Time.** Tolerated skew is five minutes. A `KRB_AP_ERR_SKEW` carrying the
  KDC's time is retried once with that offset; the offset only moves our own
  timestamps, so a forged one cannot make a forged ticket acceptable.
- **Credentials live in memory only**, owned by the consumer (`smbfs` holds
  them per uid and principal): the password-derived key is zeroed as soon as
  the AS exchange completes, a TGT on expiry or the session's end, and nothing
  is ever written to disk. There is no `kinit`/`klist` yet because there is no
  second consumer to share a cache with; one arrives with that consumer.

## 1. Stages

**K1.** `lib/resolver` gains `SRV` (the `lib/net::dns` codec already carries
the type for mDNS), ordered selection, and the bounds of the existing answer
path; host tests and a QEMU vertical against the in-tree DNS peer.

**K2.** Types and crypto with RFC 3962 and RFC 8009 vectors, and the RFC 3961
n-fold vectors; every decoder fuzzed.

**K3.** The exchanges against the in-tree KDC (`plans/SMB.md` SMB12): wrong
realm, unknown principal, wrong password, skew, referral, oversized replies,
replayed and tampered replies, a KDC offering only refused enctypes.

**K4.** RFC 4121 token framing and the checksum, mutual authentication, and
the session key handed to `lib/smb` — the AP-REP subkey when present, else the
authenticator subkey, else the ticket's (MS-KILE §3.1.1.2). It joins
`lib/smb`'s `client::Mechanism` beside NTLM, offered under Microsoft's and the
IETF's Kerberos OIDs as the server's hints name them, with its MIC token as
the mechListMIC and the AP-REP read from the final `SESSION_SETUP` reply. The
client's setup then admits a success at the first leg — NTLM's takes two — and
takes the session id from that final response.
