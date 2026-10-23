# IMAP design (RFC 9051 & RFC 3501 compatibility)

**Status:** Phase 5 in progress. The server, session, authentication, TLS, the **mailbox**
commands and `FETCH`, `SEARCH`, `STORE`, `EXPUNGE`, `COPY` and `MOVE` — each with its `UID`
variant — are implemented and tested, as are `APPEND` and `IDLE` with unsolicited mailbox updates.
`NAMESPACE` (RFC 2342), `ENABLE` (RFC 5161), `ESEARCH` (RFC 9051 §6.4.4), `LIST-EXTENDED` (RFC 5258)
and `SPECIAL-USE` (RFC 6154) are implemented and advertised; no parsed-but-refused command remains. `ENABLE CONDSTORE` succeeds (ADR-059); `QRESYNC` is
recognised and refused by its absence from `* ENABLED` (ADR-056).
This is **not** a claim of RFC 9051 compliance. See [Scope](#scope) for details.

Sandesh implements IMAP4rev2 itself (ADR-002) rather than wrapping a codec crate;
`tests/architecture.rs` bans `imap-codec` and `imap-types` so that stays true.

---

## 1. Scope

### Implemented and tested

| Command | RFC 9051 / RFC 3501 | Notes |
| --- | --- | --- |
| `CAPABILITY` | §6.1.1 | Advertises `IMAP4rev1` and `IMAP4rev2`; never advertises what is unimplemented |
| `NOOP` | §6.1.2 | Flushes updates / keeps session alive |
| `LOGOUT` | §6.1.3 | Untagged `BYE` precedes the tagged `OK` |
| `ID` | RFC 2971 | Server name only; no version disclosure (§3.3) |
| `STARTTLS` | §6.2.1 | Real upgrade; session state discarded across it |
| `AUTHENTICATE` | §6.2.2 | SASL `PLAIN` and `LOGIN`, with SASL-IR (RFC 4959) |
| `LOGIN` | §6.2.3 | Permits cleartext in developer mode (`AuthMode::Disabled`) |
| `SELECT` | §6.3.2 | Reports EXISTS, FLAGS, PERMANENTFLAGS, UIDVALIDITY, UIDNEXT, UNSEEN; `[READ-WRITE]` |
| `EXAMINE` | §6.3.3 | As `SELECT` but `[READ-ONLY]`, with empty `PERMANENTFLAGS` |
| `LIST` | §6.3.9 | `%`/`*` wildcards, reference resolution, special-use attributes, deterministic order |
| `STATUS` | §6.3.11 | `MESSAGES`, `UIDNEXT`, `UIDVALIDITY`, `UNSEEN`, `SIZE`; unsupported items are `BAD` |
| `CREATE` | §6.3.4 | Creates missing parents; `INBOX` refused; name validated by the domain |
| `DELETE` | §6.3.5 | `INBOX` refused; clears the selection if the deleted mailbox was selected |
| `RENAME` | §6.3.6 | Carries descendants; **preserves UIDVALIDITY**; `INBOX` refused |
| `SUBSCRIBE` / `UNSUBSCRIBE` | §6.3.7 / §6.3.8 | Persisted through `MailboxStore`, not per-session |
| `CLOSE` / `UNSELECT` | §6.4.2 / §6.4.3 | Returns session to Authenticated state |
| `FETCH` | §6.4.5 | Sequence sets resolved through `SequenceMap`; see [§10](#10-fetch) |
| `UID FETCH` | §6.4.8 | Identifiers resolved by UID within the selected mailbox only |
| `STORE` | §6.4.6 | `FLAGS`/`+FLAGS`/`-FLAGS` and `.SILENT`; see [§11](#11-store) |
| `UID STORE` | §6.4.8 | Resolves by UID; reports the UID in its untagged `FETCH` |
| `SEARCH` | §6.4.4 | Criteria compiled to `SearchQuery`; returns sequence numbers; see [§12](#12-search) |
| `UID SEARCH` | §6.4.8 | Same criteria; returns UIDs |

`RECENT` is deliberately **not** reported by `SELECT` or `STATUS`: RFC 9051 removed it from
IMAP4rev2 and this server keeps no `\Recent` state, so a client asking for it via `STATUS` gets
`BAD` rather than a fabricated `0`.

### `ENABLE` and what is actually enableable

`ENABLE` (RFC 5161) is implemented and advertised. `CONDSTORE` is implemented, enableable and
advertised (ADR-059). `QRESYNC` is recognised as a name a client may send and is refused, because
it is not implemented — RFC 5161 §3.1 requires an unimplemented capability to be ignored rather
than rejected, so the command answers `OK` and the capability's absence from `* ENABLED` is the
refusal. `QRESYNC` therefore stays out of `CAPABILITY` (ADR-056, ADR-059).

Note what `ENABLE CONDSTORE` does and does not gate. It governs *unsolicited* CONDSTORE data — the
`MODSEQ` in an untagged `FETCH` a client did not ask for — because RFC 5161 §3 entitles a client
that did not enable an extension to a parser that does not know its syntax. It does not gate data
the client explicitly requested: `FETCH 1 (MODSEQ)` is answered whether or not `ENABLE` was sent,
since refusing would make the item unusable for the many clients that never send `ENABLE`.

There is no longer a parsed-but-refused command: every verb the state machine permits is
implemented.

> Increment 3 removed placeholder `SEARCH`, `FETCH` and `STORE` handlers that an earlier increment
> had left in the session. They ignored their arguments (`FETCH` returned every message for any
> sequence set; `STORE` only ever toggled `\Seen`), so they were worse than absent: a client would
> have received confidently wrong data. Increment 4A reinstated `FETCH` properly, on `SequenceMap`,
> 4B.1 reinstated `STORE` on the same footing, and 4B.2 reinstated `SEARCH` on a shared
> `SearchQuery` domain plan.

### Deliberately absent

| Feature | Why | ADR |
| --- | --- | --- |
| `LSUB` | Removed by RFC 9051 in favour of `LIST` selection options | — |
| `IMAP4rev1` capability | Advertised alongside `IMAP4rev2` for standard client interoperability (Python `imaplib`, Odoo, Thunderbird) | ADR-042 (Revised) |
| `ESEARCH` | Implemented; MIN/MAX/ALL/COUNT, `SAVE` refused | ADR-057 |
| `LIST-EXTENDED` | Implemented; RECURSIVEMATCH/REMOTE refused | ADR-058 |
| `SPECIAL-USE` | Implemented; closed RFC 6154 vocabulary, `\All` unassigned | ADR-058 |
| QRESYNC | No durable expunged-UID record for `VANISHED (EARLIER)` | ADR-059 |
| `NAMESPACE` | Implemented; one personal namespace, shared/other `NIL` | ADR-055 |
| `ENABLE` | Implemented; `CONDSTORE` enableable, `QRESYNC` refused | ADR-056, ADR-059 |

---

## 2. Layering

```text
  parser   octets → Command                      pure, synchronous, no I/O
  state    (State, CommandKind) → Transition     pure
  response values → response lines               pure
  session  drives the three against a Connection and the storage ports
  server   binds listeners, chooses the TLS posture, spawns sessions
```

The first three layers perform no I/O, which is what makes the cases that matter most — malformed
literals, commands in the wrong state, capability truthfulness — synchronous unit tests rather than
socket choreography (ADR-040).

`sandesh-imap` depends on `sandesh-core`, `sandesh-proto` and `sandesh-config` and nothing else. It
issues no SQL and reaches stored data only through the ports the core defines; three architecture
tests assert this mechanically.

---

## 3. Session state (ADR-038)

```text
                    ┌────────────────────┐
  greeting ────────▶│ Not Authenticated  │
  (untagged OK)     └─────────┬──────────┘
                              │ AUTHENTICATE / LOGIN (ok)
                              ▼
                    ┌────────────────────┐
                    │   Authenticated    │◀──┐
                    └─────────┬──────────┘   │ CLOSE / UNSELECT,
                              │ SELECT /     │ or a failed SELECT
                              │ EXAMINE      │
                              ▼              │
                    ┌────────────────────┐   │
                    │      Selected      │───┘
                    └─────────┬──────────┘
                              │ LOGOUT
                              ▼
                    ┌────────────────────┐
                    │      Logout        │
                    └────────────────────┘
```

`State` is an enum, not a pair of booleans: "selected but not authenticated" is unrepresentable
rather than merely unlikely. One exhaustively-matched `transition(state, command)` function is the
sole authority on sequencing, so adding a command without deciding its state rules is a compile
error. `Selected { read_only }` carries `EXAMINE`'s promise, so mutating commands are refused by the
state machine rather than by each handler remembering to check.

---

## 4. Capabilities

Derived from configuration and the connection's encryption state, in one place, and recomputed on
every use so they cannot fall out of step after a `STARTTLS` upgrade.

| Connection | Advertised |
| --- | --- |
| Cleartext, no TLS configured | `IMAP4rev2 LOGINDISABLED` |
| Cleartext, `STARTTLS` available | `IMAP4rev2 STARTTLS LOGINDISABLED` |
| Encrypted, auth enabled | `IMAP4rev2 AUTH=PLAIN AUTH=LOGIN` |
| Encrypted, auth disabled | `IMAP4rev2 LOGINDISABLED` |

`LOGINDISABLED` (RFC 9051 §7.2.2) appears whenever `AUTH` is unusable on this connection, so a client
offers to enable TLS instead of prompting for a password that cannot be accepted. `STARTTLS` is
suppressed once encrypted, because renegotiating inside an established channel is what RFC 9051
§6.2.1 forbids.

---

## 5. Authentication

Every primitive is Phase 4.5's; there is no second implementation.

| Concern | Reused from |
| --- | --- |
| Mechanism parsing, payload decoding | `sandesh_proto::{Mechanism, decode_plain, decode_login_field}` |
| Password verification | `sandesh_proto::verify_optional` (Argon2id, constant-time) |
| Credential carriage | `sandesh_proto::Secret` |
| TLS policy | `sandesh_config::AuthMode::is_offered_over` |
| Account lookup | `sandesh_core::AccountStore` |

**Credentials are never accepted in the clear.** `PLAIN` and `LOGIN` both send the password, so both
are refused on an unencrypted connection — not merely unadvertised, *refused* (ADR-033). A correct
password over cleartext still fails, which a test asserts.

**Unknown users are indistinguishable from wrong passwords.** `verify_optional` performs the hash
comparison even when no account exists, so the two paths take the same time and produce the same
response (ADR-032). A test compares the two transcripts byte for byte.

**Credentials cannot be logged.** Passwords live in `Secret`, whose `Debug` *and* `Display` redact
(ADR-035). A test greps a real server's log for the plaintext password and key material and finds
neither.

Re-authentication is refused: a second `AUTHENTICATE` would let a client change identity mid-session.

---

## 6. TLS

One certificate, loaded once at startup by the composition root from `sandesh_proto::Identity`, and
injected into the server as a `TlsAcceptor`. The IMAP crate loads no certificate and builds no rustls
configuration — an architecture test bans `rustls`, `rcgen` and `rustls-pemfile` as runtime
dependencies.

### STARTTLS

```text
cleartext session ──STARTTLS──▶ tagged OK ──▶ handshake ──▶ *fresh* session
                                                    │
                                        all cleartext state discarded
```

The second session is genuinely new (ADR-036): RFC 9051 §6.2.1 requires the server to discard
everything learned before the handshake, and reusing the session object with a flag flipped would
leave that state behind. Anything the client pipelined behind `STARTTLS` is discarded unread — that
is the STARTTLS command-injection vulnerability. No greeting follows the upgrade, per the RFC.

### Implicit TLS

```text
TCP accept ──▶ handshake ──▶ greeting ──▶ session
```

The handshake completes before an IMAP session exists, so the greeting is the first thing written
*inside* TLS. A plaintext client fails the handshake and is dropped without reaching the protocol,
which is the property that makes the mode meaningful (ADR-037). There is no fallback to plaintext,
and a test asserts a cleartext probe receives no IMAP at all.

A configured TLS posture with no certificate **fails startup** rather than serving cleartext on a
port the operator believes is encrypted.

---

## 7. Literals (ADR-040, ADR-043)

```text
C: A001 LOGIN {5}
S: + 
C: alice secret
S: A001 OK ...
```

The parser stops at a literal and returns `Parsed::NeedLiteral`; the session sends the continuation
request, reads exactly that many bounded octets, and resumes. Multiple literals in one command work
by iteration.

- The declared length is checked against `max_literal_size` **before anything is allocated**, so
  `{4294967295}` costs a rejection rather than four gigabytes.
- A length that cannot fit a `usize` is malformed, not an overflow.
- `{n+}` (RFC 7888 `LITERAL+`) is recognised and **not** answered with a continuation, because the
  client is already sending the octets.
- Payloads stay `Vec<u8>`: a literal is explicitly binary, and decoding it as UTF-8 would corrupt the
  octets the product guarantees to preserve.

Nesting depth and token count are bounded too, so `((((…))))` cannot recurse until the stack is gone.

---

## 8. Configuration

```text
SANDESH_IMAP_ENABLED     whether the listener runs               (default: true)
SANDESH_IMAP_ADDR        cleartext address                       (default: 127.0.0.1:1143)
SANDESH_IMAP_TLS_ADDR    implicit-TLS address; setting it enables the port (default: off)
SANDESH_IMAP_AUTH        disabled | optional | required          (default: disabled)
SANDESH_IMAP_STARTTLS    whether STARTTLS is offered             (default: false)
```

Both listeners default to loopback. The implicit-TLS port is off by default: enabling it would
generate a certificate and bind a second port a developer did not ask for.

Limits: `max_line_length` 64 KiB, `max_literal_size` 25 MiB (matching SMTP's message limit),
`max_connections` 128 (higher than SMTP's, because IMAP clients hold connections open),
`command_timeout` 30 minutes (the RFC 9051 §5.4 minimum for an authenticated session).

Validation refuses configurations that cannot work: no listener at all, `start_tls` with no cleartext
listener, an empty hostname, a zero limit. Warnings cover the two dangerous-but-valid cases — an
externally bound listener with authentication off, and authentication enabled on a cleartext listener
with no `STARTTLS` (where credentials can never be accepted, so clients are locked out rather than
secured).

---

## 9. Known limitations

1. **A multi-message `MOVE` is not atomic across messages.** Each message moves in its own
   storage transaction, so a `MOVE` naming several can stop part way. It then reports `NO` with a
   count, together with the `[COPYUID]` pairs and `* <seq> EXPUNGE` lines for the messages that
   *did* move — never `OK`, which would claim a success it cannot back (ADR-052). Full atomicity
   would need a `move_entries` port; that is a storage change, not a protocol one.

   A same-mailbox `COPY` or `MOVE` is refused rather than duplicating a placement: the domain model
   places a message in a mailbox at most once (`CORE_SPEC.md` §7.3, ADR-050, ADR-051).

   Content searches are FTS-backed as of ADR-074, in one specific sense: the FTS5 index narrows
   which messages `BODY`/`TEXT` load, and never which ones match. RFC 9051 §6.4.4 requires
   substring semantics, so `SearchQuery::matches` still decides every result and the index only
   supplies a superset to check — which is why it uses a trigram tokenizer rather than the word
   tokenizer `STORAGE_DESIGN.md` §7 originally specified. Keys the index cannot narrow soundly
   (`TEXT`, `Bcc`, named headers, negations, short values) still load each candidate. `SEARCH
   UNSEEN` and other flag-only searches load nothing, index or not.
2. **Unsolicited updates reach idling sessions only.** A session in `IDLE` tracks the mailbox live
   (ADR-054). A session sitting at the command prompt still sees the snapshot it took at `SELECT`:
   no unsolicited line is injected mid-command, because that would desynchronise a client that is
   reading one specific response. Such a session learns about changes through its own commands or
   by re-selecting (ADR-046, as amended).
3. **`[APPENDUID]` correctness is not provable by this test suite.** The handler reads the UID
   `insert` returned, but reads the mailbox immediately beforehand, so `uid_next` and the assigned
   UID always coincide single-threaded — a mutation swapping them survives. See ADR-053.
4. **`RECENT` is never reported** — deliberate, see [Scope](#1-scope).
6. **Some `FETCH` items are unimplemented** and are answered `BAD` rather than faked: `BINARY[...]`,
   `BINARY.SIZE`, `MODSEQ`, `EMAILID`, `THREADID`. `BODYSTRUCTURE` reports `NIL` for per-part MD5
   and for multipart parameters, and describes a `message/rfc822` part as a leaf rather than with
   the nested-envelope form. See `PROTOCOL_CAPABILITIES.md` §4.2.
7. **Modern mail clients are partly tested.** Unmodified Python `imaplib` completes the full
   read-mail flow, and `openssl s_client` exercises the TLS upgrade. Thunderbird, `mutt` and
   `mbsync` are expected to work but are **not tested and not claimed**.

---

## 10. FETCH

`FETCH` is the largest data path in the server, and its correctness rests on four separations.

### Identifier resolution

```text
FETCH <set>          UID FETCH <set>
      │                    │
      ▼                    ▼
 SequenceSet          SequenceSet
      │ resolve()          │ resolve_uids()
      ▼                    ▼
        SequenceMap  (ADR-044, built at SELECT)
                 │
                 ▼
          MessageEntry — UID, flags, internal date
```

Both modes go through the one `SequenceMap`. Nothing in `FETCH` re-derives the ordering, and no
database row id is ever used as a sequence number or a UID. Because the map holds only the selected
mailbox's placements, **mailbox scoping is structural**: a UID naming a message in another mailbox
resolves to nothing, rather than being excluded by a check that could be forgotten.

`*` means the largest sequence number in sequence mode and the largest UID *in use* in UID mode —
not `UIDNEXT - 1`, which could name a UID that never existed. Out-of-range members are not errors
(§6.4.8): `FETCH 99` on a three-message mailbox succeeds with no data.

### Content: ranges, not copies

```text
Message raw bytes  (the authoritative octets)
        │
        ▼
MimePart::body_range / header_range   (ByteRange, from the core MIME model)
        │
        ▼
   exact sub-slice
        │
        ▼
    IMAP literal  {n}CRLF + n octets
```

Every body section is a slice of the stored message. Nothing is decoded, re-encoded or normalised,
so CRLF, bare LF, 8-bit octets, duplicate headers, header order and base64 payloads all survive
unchanged. The MIME tree comes from `sandesh-core`; there is no second MIME parser in this crate,
and `tests/architecture.rs` enforces that mechanically.

`BODY[HEADER.FIELDS (...)]` is the one section that is *assembled* rather than sliced — a field
subset has no single range — but it is assembled **from** per-field ranges (`Header::range`), so
each field's own bytes, folding and encoded words are still copied verbatim.

### Literals

A section value is always a literal, never a quoted string, because content is arbitrary octets and
a literal is the only IMAP form with no escaping. The length prefix is read from the same slice that
is appended, in `ResponseBuilder::literal`, so a declared count cannot disagree with its payload.

`BODY[section]<offset.count>` applies its window last, to the bytes that would otherwise have been
sent. The window is computed with checked slicing: an offset past the end yields `{0}`, a count
longer than what remains yields only what remains, and no combination of `u32` values can index out
of bounds.

### `BODY` versus `BODY.PEEK`

`BODY[...]` sets `\Seen`; `BODY.PEEK[...]` does not (§6.4.5). The flag is set through
`MessageStore::update_flags` — the same port `STORE` will use — never by touching SQLite from this
crate. It is written *before* the response is rendered, so a client asking for `(FLAGS BODY[])` is
told the message is seen, which is what is now stored.

`RFC822` and `RFC822.TEXT` set `\Seen`; `RFC822.HEADER` does not, being defined as
`BODY.PEEK[HEADER]`. No `FETCH` macro (`ALL`, `FAST`, `FULL`) sets it, since none includes a
content section. An `EXAMINE`d mailbox never has flags changed at all.

### Efficiency

The raw message is loaded once per message per command, only when an item actually needs it — a
`FETCH 1:* (UID FLAGS)` reads no message bodies. Untagged lines are written without flushing and
the tagged completion flushes once, so a multi-message `FETCH` is one syscall batch rather than one
per message. The response buffer is pre-sized from the message's known length.

---

## 11. STORE

`STORE` is the first command that *changes* stored mail, so its design is mostly about what it
does not do.

```text
sequence set ──▶ SequenceMap ──▶ MessageEntry.id   (one placement)
                                       │
                                       ▼
                    MessageStore::update_flags     (one transaction;
                                       │            emits EntryFlagsChanged)
                                       ▼
                  snapshot updated  +  untagged FETCH FLAGS
                                       (suppressed by .SILENT)
```

### One mutation path

The three operations map one-to-one onto `FlagUpdate::{Replace, Add, Remove}`, and the storage
layer applies them inside a single transaction. This adapter computes no resulting flag set of its
own: the value it reports is the value `update_flags` returned. There is deliberately no second
flag-mutation path — `FETCH`'s implicit `\Seen` uses the same port.

### Placement scoping is structural

The mutation targets `MessageEntry.id`, which identifies **one placement**, not a message. A
message that sits in two mailboxes has two placements with independent flags, so a `STORE` in
`INBOX` cannot affect the `Archive` copy — not because of a check, but because the id names only
one row. Identifier resolution goes through the selected mailbox's `SequenceMap` (ADR-044), so a
UID belonging to another mailbox resolves to nothing.

### `.SILENT` suppresses the report, not the change

RFC 9051 §6.4.6: a non-`.SILENT` `STORE` reports the resulting flags as an untagged `FETCH`, with
the UID included in UID mode (§6.4.8). `.SILENT` omits that line. The flags change either way, and
the session's snapshot is updated either way — the update happens *before* the silence check, so a
silent store cannot leave the session stale.

### Nothing is partially applied

The sequence set and the flag argument are both parsed before any entry is touched. A malformed
set, an unknown operation, or an unsupported flag therefore mutates nothing and emits no event.
`\Recent` and `\*` are refused rather than ignored (ADR-045): a client must not believe it set a
flag the server cannot store.

### Read-only selections

The state machine refuses `STORE` on an `EXAMINE`d selection before the handler runs, and the
handler re-checks. `EXAMINE` reports empty `PERMANENTFLAGS`, so a well-behaved client does not try.

### Same-session freshness (ADR-047)

A successful mutation writes the new flags back into the acting session's snapshot, so a subsequent
`FETCH FLAGS` in that session is never stale. Only flags are written back — membership and sequence
numbering stay fixed at `SELECT` (ADR-046) — and only for the acting session. Another session sees
the change after it re-selects; no unsolicited responses are sent.

---

## 12. SEARCH

`SEARCH` is the first command whose logic is genuinely shared with another surface, so its design is
mostly about where that logic lives.

```text
RFC 9051 keys ──▶ CompiledSearch ──▶ SearchQuery        (sandesh-core, pure)
                       │                   │
                       │                   ├──▶ matches(Option<&Message>, &MessageEntry)
                       │                   │
                       └── sequence sets ──┘  resolved via SequenceMap
                                               │
                                               ▼
                            * SEARCH <sequence numbers | UIDs>
```

### One plan, two surfaces

`STORAGE_DESIGN.md` §7 specifies that IMAP `SEARCH` and the REST/UI filter syntax compile into one
`SearchQuery` domain type, so the two cannot answer the same question differently. That type now
exists in `sandesh-core`; this adapter only compiles keys into it and names the results. No SQL, no
mailbox filtering and no matching logic live in `sandesh-imap`.

### Scope is structural

The candidate set is the session's `SequenceMap` — the selected mailbox's placements and nothing
else. A message in another mailbox is therefore unreachable by construction rather than excluded by
a filter, and no query a client can write widens that.

### What each mode returns

`SEARCH` returns sequence numbers; `UID SEARCH` returns UIDs. Both come from the same
`MessageEntry`, so they can only disagree about *how* to name a match, never about which messages
matched. Results are ascending and deduplicated because they are produced by walking the mailbox in
order, not by sorting afterwards.

A bare message set is read in the command's own mode — sequence numbers for `SEARCH`, UIDs for
`UID SEARCH`. An explicit `UID <set>` always means UIDs. Sequence sets cannot be expressed as a
`SearchQuery` (a position is not a property of a message), so they are returned alongside the query
and resolved against the snapshot.

### Cost

`SearchQuery::needs_message` reports whether a query can be answered from the placement alone.
Flag, keyword and UID criteria can, so `SEARCH UNSEEN` over a large mailbox reads no message bodies
at all. Only a content or size key loads messages, and then one per candidate.

### Snapshot

Search membership is the acting session's snapshot (ADR-046), and it sees that session's own flag
changes immediately (ADR-047) — so `STORE` followed by `SEARCH` in one session is consistent.
Another session's changes appear after a re-`SELECT`. `SEARCH` mutates nothing, not even `\Seen`, so
it is permitted on an `EXAMINE`d selection.
