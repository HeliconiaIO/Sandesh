# Sandesh — Protocol Capabilities

**Status:** Largely implemented. SMTP (Phase 2), IMAP (Phase 5) and POP3 (Phase 6) are built
and tested; per-row phase markers below say when each capability was or will be delivered.

Phase numbers refer to [ROADMAP.md](ROADMAP.md). Legend: **✓** in scope for that phase · **○**
deferred past v1 · **—** not applicable to that protocol · **✗** deliberately out of scope.

---

## 1. Normative references

Design targets, in order of authority. Primary sources only.

| RFC | Title | Relevance |
| --- | --- | --- |
| 5321 | SMTP | Core SMTP |
| 5322 | Internet Message Format | Message syntax |
| 2045–2049 | MIME | Structure, encodings, media types |
| 6152 | 8BITMIME | Required by modern senders |
| 3030 | CHUNKING / BDAT | Used by some clients |
| 1870 | SIZE extension | Advertise message limits |
| 3207 | SMTP STARTTLS | Opportunistic TLS |
| 4954 | SMTP AUTH | Authentication |
| 6531 | SMTPUTF8 | Internationalised addresses; deferred, refused with `504` |
| 9051 | **IMAP4rev2** | Primary IMAP target |
| 3501 | IMAP4rev1 | Compatibility target for older clients |
| 2177 | IMAP IDLE | Push notification |
| 6851 | IMAP MOVE | Atomic move |
| 4315 | UIDPLUS | `APPENDUID`/`COPYUID` |
| 6154 | SPECIAL-USE | Sent/Drafts/Trash discovery; `CREATE (USE …)` (ADR-058) |
| 2342 | NAMESPACE | Personal namespace discovery (ADR-055) |
| 5258 | LIST-EXTENDED | Selection and RETURN options (ADR-058) |
| 9051 §6.4.4 | ESEARCH | `SEARCH RETURN (…)`; `SAVE` refused (ADR-057) |
| 5161 | ENABLE | Implemented; `CONDSTORE` is enableable (ADR-056, ADR-059) |
| 4959 | SASL-IR | Initial response |
| 7162 | CONDSTORE/QRESYNC | CONDSTORE implemented (ADR-059); QRESYNC deferred, no expunged-UID record |
| 1939 | POP3 | Core POP3 |
| 2449 | POP3 CAPA | Capability announcement |
| 2595 | POP3 STLS | POP3 over TLS |
| 5034 | POP3 SASL AUTH | Authentication |
| 4422 | SASL | Auth framework |
| 4616 | PLAIN mechanism | Primary mechanism |
| 8314 | Implicit TLS for mail | Implicit-TLS ports |

## 2. Cross-protocol capability matrix

| Capability | SMTP | IMAP | POP3 | REST | Phase |
| --- | :--: | :--: | :--: | :--: | --- |
| Accept message (ingress) | ✓ | ✓ APPEND | — | ✓ | 2 / 5 / — / 3 |
| Read message content | — | ✓ | ✓ | ✓ | 5 / 6 / 3 |
| Retrieve exact raw bytes | — | ✓ | ✓ | ✓ | 5 / 6 / 3 |
| Partial/section fetch | — | ✓ | ✓ TOP | ✓ | 5 / 6 / 3 |
| Delete message | — | ✓ | ✓ | ✓ | 5 / 6 / 3 |
| Folders / mailboxes | — | ✓ | ✗ (INBOX only) | ✓ | 5 / — / 3 |
| Flags | — | ✓ | ✗ | ✓ | 5 / — / 3 |
| Server-side search | — | ✓ | ✗ | ✓ | 5 / — / 3 |
| Copy / move | — | ✓ | ✗ | ✓ | 5 / — / 3 |
| Push notification | — | ✓ IDLE | ✗ | ✓ WebSocket | 5 / — / 3 |
| Authentication | ✓ | ✓ | ✓ | ✓ | 7 |
| STARTTLS | ✓ | ✓ | ✓ | — | 7 |
| Implicit TLS port | ✓ | ✓ | ✓ | ✓ HTTPS | 7 |
| Multi-account | ✓ routing | ✓ | ✓ | ✓ | 1 |
| Persistent unique id | — | ✓ UID | ✓ UIDL | ✓ | 5 / 6 / 3 |
| UTF-8 / i18n addresses | ○ SMTPUTF8 | ✓ | ○ | ✓ | — / 5 / — / 3 |
| Fault injection | ✓ | ○ | ○ | — | 9 |

Note the asymmetry: POP3 has no folder, flag, or search concept by design (RFC 1939). Those are not
gaps to fill; a POP3 client sees a flat INBOX snapshot. This is why the domain model cannot be shaped
by any single protocol.

## 3. SMTP

**Status: implemented and tested (Phase 2).** See [SMTP_DESIGN.md](SMTP_DESIGN.md).

Sandesh is an **ingress-only** server: it accepts mail and delivers locally. It is never an MTA and
never relays by default.

### 3.1 Commands

| Command | Phase | Notes |
| --- | :--: | --- |
| `EHLO` | 2 | Primary greeting; returns the extension list |
| `HELO` | 2 | Legacy fallback |
| `MAIL FROM` | 2 | Null reverse-path `<>` accepted (bounce testing) |
| `RCPT TO` | 2 | Multiple recipients; routing per CORE_SPEC §7 |
| `DATA` | 2 | Dot-stuffing removed; raw bytes preserved exactly |
| `RSET` | 2 | Aborts the transaction, keeps the connection |
| `NOOP` | 2 | |
| `QUIT` | 2 | |
| `VRFY` | 2 | Returns `252` (cannot verify) — the correct non-disclosing answer |
| `EXPN` | 2 | `502` not implemented |
| `HELP` | 2 | Brief command list |
| `AUTH` | 7 | PLAIN, LOGIN |
| `STARTTLS` | 7 | RFC 3207 |
| `BDAT` | **8** | CHUNKING (RFC 3030); implemented in A4, ADR-088 |

### 3.2 Advertised extensions

| Extension | Phase | Notes |
| --- | :--: | --- |
| `8BITMIME` | 2 | Required by modern senders |
| `SIZE` | 2 | Advertises `max_message_size` (default 25 MiB) |
| `PIPELINING` | 2 | Straightforward and expected |
| `ENHANCEDSTATUSCODES` | 2 | RFC 3463; better client diagnostics |
| `SMTPUTF8` | 7 | RFC 6531 |
| `AUTH` | 7 | Mechanism list |
| `STARTTLS` | 7 | |
| `CHUNKING` | **8** | Advertised since A4, when `BDAT` landed (ADR-088) |
| `DSN` | ○ | Deferred |

### 3.2a Implementation status

Every Phase 2 row above is implemented and covered by tests. Two clarifications:

* **`SMTPUTF8`** is not advertised and is refused with `504` rather than silently accepted, so a
  client learns the parameter had no effect instead of assuming it did.
* **`BODY=7BIT` and `BODY=8BITMIME`** are both accepted. Content is stored as bytes either way, so
  the declaration changes nothing about handling — it only needs to not be rejected.

`BDAT`/`CHUNKING` shipped in Phase 8 A4 (ADR-088). `AUTH` and `STARTTLS` shipped in Phase 4.5
(ADR-016, ADR-036, ADR-037). Still deferred: `SMTPUTF8` and `DSN` (unscheduled) — both remain
unadvertised, which `crates/sandesh-smtp/tests/protocol.rs` asserts.

### 3.3 Behavioural decisions

- **Accept, do not validate.** Malformed messages are accepted and stored. A tool for testing
  malformed mail that rejects malformed mail is useless. Rejection happens only for transport-level
  faults (size limit, unknown recipient in `strict` mode) or deliberate fault injection.
- **No relaying.** Every accepted message is delivered locally. Relay is a future explicit opt-in.
- **Dot-stuffing** is reversed during DATA (RFC 5321 §4.5.2); the stored bytes are the true message.
- **Line endings are preserved.** A client sending bare `\n` gets bare `\n` back from IMAP FETCH.
  Normalising would destroy the fidelity that justifies the product.
- **Full transaction traces** are recorded per connection for the Diagnostics view.

## 4. IMAP

The largest implementation and the main reason for the multi-phase roadmap.

**Baseline decision:** target **IMAP4rev2 (RFC 9051)** while advertising `IMAP4rev1` for
compatibility. rev2 folds UIDPLUS, MOVE, IDLE, SPECIAL-USE, LIST-EXTENDED and ENABLE into the base
spec, so aiming there yields a coherent modern feature set rather than an arbitrary pile of
extensions. Clients that only speak rev1 get rev1 behaviour; the difference is negotiated with
`ENABLE`. Recorded as ADR-004.

### 4.0 Implementation status (Phase 5, increments 1–4B.2)

**Implemented and tested:** `CAPABILITY`, `NOOP`, `LOGOUT`, `ID`, `STARTTLS`, `AUTHENTICATE`
(PLAIN/LOGIN, with SASL-IR), `LOGIN`. Mailbox operations: `SELECT`, `EXAMINE`, `LIST`, `STATUS`,
`CREATE`, `DELETE`, `RENAME`, `SUBSCRIBE`, `UNSUBSCRIBE`, `CLOSE`, `UNSELECT`. Message retrieval:
`FETCH` and `UID FETCH` (increment 4A) — see §4.2 for exactly which data items — and `STORE` /
`UID STORE` (increment 4B.1), including `FLAGS`, `+FLAGS`, `-FLAGS` and every `.SILENT` variant,
and `SEARCH` / `UID SEARCH` (increment 4B.2) — see §4.3 for the criteria. Two listeners —
cleartext with optional `STARTTLS`, and implicit TLS — over real sockets.

**A client can now read mail over IMAP.** Verified end to end: mail delivered over SMTP is
retrievable, byte-exact, through `SELECT` + `FETCH`, and unmodified Python `imaplib` completes
connect → `LOGIN` → `LIST` → `SELECT` → `FETCH` metadata → `FETCH` body → `LOGOUT`.

**A client can now delete mail over IMAP.** `EXPUNGE` and `UID EXPUNGE` (RFC 4315 §2.1) remove
`\Deleted` placements, report each removal as `* <seq> EXPUNGE` with its pre-removal sequence
number, and renumber the acting session's own view so a following `FETCH`/`SEARCH`/`STORE`
addresses the mailbox as it now is (ADR-049).

**A client can now copy mail between mailboxes.** `COPY` and `UID COPY` create new placements of
the shared message with fresh destination UIDs and report `[COPYUID <dest uidvalidity> <source set>
<dest set>]` (RFC 4315 §3), using the UIDs storage assigned rather than computed ones. The source
mailbox — its UIDs, flags and sequence numbering — is untouched, and `COPY` is legal on an
`EXAMINE`d selection because it writes only to the destination (ADR-050). Copying a message into
the mailbox it already sits in is refused: the domain model allows one placement per mailbox.

**A client can now move mail, upload it, and be told when it changes.** `MOVE` and `UID MOVE`
(RFC 6851) create the destination placement, remove the source, report `[COPYUID]` and the
`* <seq> EXPUNGE` lines, and update the acting session's sequence numbering. `APPEND` (RFC 9051
§6.3.12) uploads a message with optional flags and `INTERNALDATE`, stores the submitted octets
byte-for-byte, and returns `[APPENDUID <uidvalidity> <uid>]` naming the UID storage assigned.
`IDLE` (RFC 2177) delivers unsolicited `EXISTS`, `EXPUNGE` and flag updates as they happen.

Moving a message into the mailbox it already sits in is refused, for the same reason `COPY` is
(ADR-051). A multi-message `MOVE` that only partly succeeds reports `NO` together with the moves
that did happen, rather than claiming a success it cannot back (ADR-052).

`NAMESPACE` (RFC 2342) reports exactly one personal namespace, `(("" "/"))`, with `NIL` for both
the shared and other-users namespaces. That shape is dictated by the mailbox model rather than
chosen: `MailboxStore::list` takes an `AccountId` and returns that account's mailboxes and nothing
else, so every name a client can reach is personal and needs no prefix. `NIL` rather than an empty
list `()` is the deliberate distinction RFC 2342 §5 draws — `NIL` means "this server has none",
while `()` would claim namespaces exist that no `LIST` could enumerate.

`ENABLE` (RFC 5161) is implemented and advertised. `CONDSTORE` is enableable; `QRESYNC` is
recognised as a name a client may send and refused, because it is not implemented (see §4.4). RFC 5161 §3.1 requires an unimplemented capability to
be ignored rather than rejected, so the command answers `OK` with an empty `* ENABLED` line — the
absence from that line *is* the refusal. It is Authenticated-state only: refused before login, and
refused again once a mailbox is selected, because enabling an extension can change the syntax of
untagged responses a selected mailbox is already streaming.

**Not implemented:** every command the state machine permits is now implemented; there is no
longer a parsed-but-refused verb.

> **Fetching agents now work, including deletion.** Both loops Odoo `fetchmail` performs complete
> end to end, verified against a running server and through unmodified Python `imaplib`: the
> keep-mail loop `SELECT` → `SEARCH UNSEEN` → `FETCH` → `STORE +FLAGS \Seen`, and the delete loop
> `SELECT` → `SEARCH` → `FETCH` → `STORE +FLAGS \Deleted` → `EXPUNGE` → `SEARCH`/`FETCH`. After the
> `EXPUNGE` the same session sees the renumbered mailbox, so an agent that re-reads immediately
> does not reprocess the message it just removed (ADR-049).

**Snapshot semantics (ADR-046, as amended by ADR-054).** A selection is a snapshot taken at
`SELECT`. A session that is **idling** now tracks the mailbox live: arrivals, removals and flag
changes made by *any* session are delivered as unsolicited responses and applied to the idling
session's own sequence mapping, so its numbering and the client's stay in step (ADR-054). A session
that is **not** idling still sees the snapshot it took: a message that arrives afterwards becomes
visible when it re-selects, and no unsolicited responses are injected mid-command. A `FETCH` never fails because another session changed the
mailbox: metadata is answered from the snapshot, and a message whose content has since been
removed is omitted from the response.

**Advertised today:** `IMAP4rev1`, `IMAP4rev2`, plus `STARTTLS` when available, `AUTH=PLAIN`/`AUTH=LOGIN`
when usable on the connection, and `LOGINDISABLED` only when authentication is required over cleartext.
In developer mode (`AuthMode::Disabled`), cleartext `LOGIN` is permitted without `LOGINDISABLED`.

**Compatibility (ADR-042 Revised):** `IMAP4rev1` is advertised alongside `IMAP4rev2` per RFC 9051 §6.1.1
so standard libraries and mail user agents (such as Python `imaplib`, Odoo `fetchmail`, Thunderbird) can
connect, authenticate, inspect mailboxes **and read mail** without erroring on the capability line.
Interoperability is verified with raw sockets, `openssl s_client`, and unmodified Python `imaplib`
(which needs no local patch precisely because the rev1 name is advertised);
Thunderbird, `mutt` and `mbsync` are **not** yet tested and no compatibility claim is made for them.

See [IMAP_DESIGN.md](IMAP_DESIGN.md) for the full scope and the session/TLS/auth design.

### 4.1 Commands by state

**Any state**

| Command | Phase | Notes |
| --- | :--: | --- |
| `CAPABILITY` | 5 | |
| `NOOP` | 5 | Also delivers pending untagged updates |
| `LOGOUT` | 5 | |

**Not authenticated**

| Command | Phase | Notes |
| --- | :--: | --- |
| `STARTTLS` | 7 | |
| `AUTHENTICATE` | 7 | PLAIN, LOGIN; SASL-IR supported |
| `LOGIN` | 5 | Phase 5 runs with auth disabled by default; enforced in Phase 7 |

**Authenticated**

| Command | Phase | Notes |
| --- | :--: | --- |
| `SELECT` | 5 | Returns EXISTS, UIDNEXT, UIDVALIDITY, FLAGS, PERMANENTFLAGS, LIST |
| `EXAMINE` | 5 | Read-only SELECT |
| `CREATE` | 5 | Implicit parent creation |
| `DELETE` | 5 | |
| `RENAME` | 5 | Includes descendants; INBOX special case |
| `SUBSCRIBE` / `UNSUBSCRIBE` | 5 | |
| `LIST` | 5 | With SPECIAL-USE, SUBSCRIBED, CHILDREN and STATUS options (ADR-058) |
| `LSUB` | — | **Not implemented.** Removed by RFC 9051; use `LIST (SUBSCRIBED)` |
| `NAMESPACE` | 5 | RFC 2342; single personal namespace `("" "/")`, shared/other `NIL` |
| `SEARCH RETURN (…)` | 5 | ESEARCH: MIN, MAX, ALL, COUNT; `SAVE` refused (ADR-057) |
| `LIST (SUBSCRIBED)` / `(SPECIAL-USE)` | 5 | Selection options; RECURSIVEMATCH/REMOTE refused |
| `LIST … RETURN (…)` | 5 | SUBSCRIBED, CHILDREN, STATUS (ADR-058) |
| `STATUS` | 5 | MESSAGES, UIDNEXT, UIDVALIDITY, UNSEEN, SIZE; `RECENT` refused (no \Recent state) |
| `APPEND` | 5 | With flags and INTERNALDATE; returns `APPENDUID` |
| `ENABLE` | 5 | RFC 5161; enables `CONDSTORE`; `QRESYNC` refused |
| `IDLE` | 5 | Wired to the event bus |
| `UNSELECT` | 5 | RFC 3691 |

**Selected**

| Command | Phase | Notes |
| --- | :--: | --- |
| `FETCH` | 5 | See §4.2 |
| `UID FETCH/STORE/COPY/MOVE/SEARCH/EXPUNGE` | 5 | UID variants of all applicable commands |
| `STORE` | 5 | `FLAGS`, `+FLAGS`, `-FLAGS`, `.SILENT` forms |
| `SEARCH` | 5 | See §4.3 |
| `COPY` | 5 | Returns `COPYUID` |
| `MOVE` | 5 | RFC 6851 |
| `EXPUNGE` | 5 | |
| `UID EXPUNGE` | 5 | RFC 4315 |
| `CLOSE` | 5 | Expunges and deselects |
| `SEARCH RETURN` | 5 | ESEARCH: MIN/MAX/ALL/COUNT |
| `SORT` / `THREAD` | ○ | RFC 5256; deferred |

### 4.2 FETCH data items

Implemented in increment 4A (● = implemented and tested, ○ = deferred):

| Item | Status | Notes |
| --- | :--: | --- |
| `UID` | ● | The placement's own UID, scoped to the selected mailbox |
| `FLAGS` | ● | The stored flags, system and custom keywords; deterministic order |
| `INTERNALDATE` | ● | The stored timestamp, RFC 9051 §9 `date-time`, space-padded day |
| `RFC822.SIZE` | ● | Exact octet count of the stored raw bytes |
| `ENVELOPE` | ● | From parsed headers, with the §7.5.2 sender/reply-to fallbacks |
| `BODYSTRUCTURE` | ● | From the core MIME tree, with extension data |
| `BODY` (no section) | ● | The same tree without extension data |
| `BODY[]`, `BODY[HEADER]`, `BODY[TEXT]` | ● | Byte-exact slices of the raw message |
| `BODY[<part>]`, `BODY[<part>.MIME]` | ● | Slices at the MIME model's byte ranges |
| `BODY[<part>.HEADER]`, `BODY[<part>.TEXT]` | ● | Slices within the part's body |
| `BODY[HEADER.FIELDS (...)]` / `.NOT` | ● | Assembled from per-field ranges, verbatim |
| `BODY[...]<offset.count>` | ● | Byte-range window; bounded, never out of range |
| `BODY.PEEK[...]` | ● | Does not set `\Seen` |
| `RFC822`, `RFC822.HEADER`, `RFC822.TEXT` | ● | Aliases; `RFC822.HEADER` peeks, the others do not |
| `ALL`, `FAST`, `FULL` macros | ● | Expanded per §6.4.5; none sets `\Seen` |

**`STORE` items (increment 4B.1).**

| Item | Status | Notes |
| --- | :--: | --- |
| `FLAGS` / `FLAGS.SILENT` | ● | Replaces the whole set; `FLAGS ()` clears everything |
| `+FLAGS` / `+FLAGS.SILENT` | ● | Adds, leaving other flags untouched |
| `-FLAGS` / `-FLAGS.SILENT` | ● | Removes, leaving other flags untouched |
| System flags | ● | `\Seen`, `\Answered`, `\Flagged`, `\Deleted`, `\Draft` |
| Custom keywords | ● | Open set, e.g. `$Junk`, `$Forwarded` |
| `\Recent`, `\*` | ○ | Refused (`BAD`), never silently ignored — see ADR-045 |
| `UNCHANGEDSINCE` modifier | ● | CONDSTORE; `MODIFIED` on refusal |

A non-`.SILENT` `STORE` reports the resulting flags as an untagged `FETCH`, carrying the UID in
UID mode (§6.4.8). `.SILENT` suppresses that report but **still changes the flags**. `STORE` on an
`EXAMINE`d (read-only) selection is refused with no mutation.
| `BINARY[...]` / `BINARY.SIZE` | ○ | RFC 3516 / RFC 9051 §6.4.5; deferred, and rejected rather than faked |
| `MODSEQ` | ● | CONDSTORE (RFC 7162 §3.1.4); parenthesised value |
| `EMAILID` / `THREADID` | ○ | RFC 8474 `OBJECTID`; deferred, and rejected |

**Not full RFC 9051 `FETCH` compliance.** `BINARY[...]`, `BINARY.SIZE`, `EMAILID` and
`THREADID` are unimplemented and are answered `BAD` rather than with a fabricated value. Two
further limits: `BODYSTRUCTURE` reports `NIL` for a part's MD5 (the stored digest covers the whole
message, not each part) and `NIL` for multipart body parameters, and a `message/rfc822` part is
described as a leaf rather than with the nested-envelope form §7.5.2 defines.

### 4.3 SEARCH keys

Implemented in increment 4B.2 (● = implemented and tested, ○ = refused, never faked):

| Group | Keys | Status |
| --- | --- | :--: |
| Sequence | `ALL`, bare sequence sets, `UID <set>` | ● |
| Flags | `SEEN`, `UNSEEN`, `ANSWERED`, `UNANSWERED`, `FLAGGED`, `UNFLAGGED`, `DELETED`, `UNDELETED`, `DRAFT`, `UNDRAFT`, `KEYWORD`, `UNKEYWORD` | ● |
| Headers | `FROM`, `TO`, `CC`, `BCC`, `SUBJECT`, `HEADER <field> <value>` | ● |
| Content | `BODY`, `TEXT` | ● |
| Dates | `BEFORE`, `ON`, `SINCE`, `SENTBEFORE`, `SENTON`, `SENTSINCE` | ● |
| Size | `LARGER`, `SMALLER` | ● |
| Logic | `NOT`, `OR`, implicit AND, parentheses | ● |
| Charset | `CHARSET UTF-8` / `US-ASCII` | ● |
| `\Recent` family | `NEW`, `OLD`, `RECENT` | ○ |
| Other charsets | anything but UTF-8 / US-ASCII | ○ |
| Extensions | `EMAILID`/`THREADID` (OBJECTID), `SAVEDATE` | ○ |

**Semantics.** `SEARCH` returns **sequence numbers**; `UID SEARCH` returns **UIDs**. Results are
ascending and deduplicated. A bare message set means sequence numbers in `SEARCH` and UIDs in
`UID SEARCH`; an explicit `UID <set>` always means UIDs in both. Text matching is a
case-insensitive substring match (RFC 9051 §6.4.4), and `BODY`/`TEXT` search the *decoded* text, so
a quoted-printable or base64 part matches as a reader would see it. Date keys compare calendar days
in UTC. A search matching nothing still emits the bare `* SEARCH` line, as §7.3.4 requires.

`NEW`, `OLD` and `RECENT` are refused rather than answered: this server keeps no `\Recent` state
(ADR-045), so any answer would be fabricated. An unsupported `CHARSET` is answered
`NO [BADCHARSET (US-ASCII UTF-8)]` per §6.4.4, which is a refusal the client can retry — not `BAD`.

**Scope.** A search only ever sees the selected mailbox's placements, because the candidate set
*is* the session's `SequenceMap`. There is no mailbox filter to omit.

**FTS-backed as a prefilter** (ADR-074). The FTS5 index behind `SearchIndex` narrows which
messages a content key loads; it never decides a match. RFC 9051 §6.4.4 defines these keys as
substring matches, so the index is a trigram index and `SearchQuery::matches` remains the
authority — an index that declines to narrow, or is absent entirely, changes only how long a
`SEARCH` takes. `SEARCH UNSEEN` and other flag-only searches read no message bodies at all,
as before.

### 4.4 Advertised capabilities

**Advertised today**, on a cleartext connection with an event source injected:

```
IMAP4rev2 IMAP4rev1 MOVE NAMESPACE ENABLE CONDSTORE ESEARCH LIST-EXTENDED SPECIAL-USE UIDPLUS
UNSELECT SASL-IR LITERAL- IDLE [STARTTLS] [AUTH=PLAIN AUTH=LOGIN | LOGINDISABLED]
```

`IDLE` appears only when the composition root supplied an `EventSource` — a server that cannot
report changes does not claim it can (ADR-053). `MOVE`, `UIDPLUS`, `NAMESPACE`, `ENABLE`, `CONDSTORE`,
`ESEARCH`, `LIST-EXTENDED`, `SPECIAL-USE`, `UNSELECT`, `SASL-IR` and `LITERAL-` are unconditional: all are implemented for every session, answered
from the mailbox model, the search engine and the session itself with no configuration or injected
dependency that could make them unanswerable.
Nothing else is advertised, because nothing else is implemented; the rule is that a capability
appears exactly when the server can honour it, and `response.rs` asserts both halves of that in
tests.

Note what advertising `ENABLE` does *not* imply. `ENABLE` being implemented is a statement about the
command, not about any extension being enableable — `QRESYNC` stays absent from `CAPABILITY`
until the storage semantics behind it exist. `LITERAL-` is advertised deliberately in place of
`LITERAL+` (RFC 7888).

Phase 5 target: `IMAP4rev2 IMAP4rev1 ENABLE IDLE NAMESPACE UIDPLUS MOVE UNSELECT SPECIAL-USE LIST-EXTENDED LITERAL+ ESEARCH SASL-IR`

Reached: every name in the target, with `LITERAL-` in place of `LITERAL+`, plus `CONDSTORE`.

Phase 7 adds: `STARTTLS AUTH=PLAIN AUTH=LOGIN` (plus `LOGINDISABLED` on cleartext ports
when required credentials are configured — mandated by RFC 9051).

Deferred: `QRESYNC`, `SORT`, `THREAD`, `BINARY`, `QUOTA`, `COMPRESS`, `OBJECTID`.

`CONDSTORE` (RFC 7162 §3) is **implemented and advertised**. Migration `0002` added
`mailboxes.highest_mod_seq`, and `mailbox_repo::allocate_mod_seq` allocates from it with a single
`UPDATE … RETURNING` inside the caller's transaction — the same shape `allocate_uid` uses, so two
concurrent writers cannot be handed the same value. Every insertion and every *effective* flag
change is stamped from it; a no-op store allocates nothing (RFC 7162 §3.1.11). The protocol surface
is `FETCH MODSEQ`, `FETCH … (CHANGEDSINCE n)` with its implied `MODSEQ` (§3.1.4.1),
`STORE … (UNCHANGEDSINCE n)` with `MODIFIED` on refusal (§3.2.5), `SEARCH MODSEQ`,
`HIGHESTMODSEQ` on `SELECT`/`EXAMINE`/`STATUS`, and the unsolicited `FETCH … MODSEQ` sent only to a
session that issued `ENABLE CONDSTORE`.

`QRESYNC` remains deferred, and **not** merely because it is larger. It needs a durable record of
*expunged* UIDs so `VANISHED (EARLIER)` can tell a returning client what disappeared while it was
away; this server keeps no such record — an expunged placement is simply gone. Advertising it would
strand a client waiting for a `VANISHED` response that never arrives, so `ENABLE QRESYNC` is
recognised and refused, and the name is absent from `CAPABILITY`.

## 5. POP3

Deliberately minimal. POP3 is a flat, single-mailbox retrieval protocol, and Sandesh implements it
faithfully rather than extending it.

| Command | Phase | Notes |
| --- | :--: | --- |
| `USER` / `PASS` | 6 | Auth enforced in Phase 7 |
| `APOP` | ○ | Requires storing a recoverable secret; deferred and probably declined |
| `STAT` | 6 | Count and total octets |
| `LIST` | 6 | With and without an argument |
| `RETR` | 6 | Exact raw bytes, dot-stuffed on the wire |
| `DELE` | 6 | Session-scoped; applied on QUIT |
| `NOOP` | 6 | |
| `RSET` | 6 | Clears pending deletes |
| `QUIT` | 6 | Enters UPDATE state; applies deletes |
| `TOP` | 6 | Headers plus N body lines |
| `UIDL` | 6 | Stable unique id; see §5.2 |
| `CAPA` | 6 | RFC 2449 |
| `STLS` | 7 | RFC 2595 |
| `AUTH` (SASL) | 7 | RFC 5034 |

### 5.1 Session model

A POP3 session takes a **snapshot of INBOX at authentication**: messages numbered 1..N, stable for
the session's life. Messages arriving mid-session are invisible until reconnection — required by RFC
1939, and the reason POP3 numbering is session state rather than a domain concept.

Deletion is staged in the session and applied to the core only on entering UPDATE state (`QUIT`). A
dropped connection discards pending deletes.

Per RFC 1939 the mailbox is exclusively locked for the session's duration. **Decision:** Sandesh does
*not* enforce an exclusive lock, because a developer will routinely have the Web UI open while
testing POP3, and locking them out would be hostile. The snapshot model already provides the
consistency the lock exists to guarantee. Documented deviation, recorded as ADR-006.

### 5.2 UIDL

`UIDL` must be persistent and unique per message. Sandesh returns a stable identifier derived from
the `MessageEntry`'s UID within INBOX, formatted as a hex string. This satisfies RFC 1939's
requirement (unique, persistent, 1–70 ASCII chars) and reuses the UID guarantees the domain already
provides.

## 6. TLS and authentication

Both are Phase 7 and shared across protocols via `sandesh-proto`.

### 6.1 TLS

| Aspect | Decision |
| --- | --- |
| Library | `rustls` — no OpenSSL dependency, keeps static-binary distribution simple |
| Modes | STARTTLS on the standard ports; optional implicit-TLS ports (RFC 8314) |
| Certificates | Auto-generated self-signed cert on first run, persisted to the data directory; overridable with a supplied cert/key |
| Minimum version | TLS 1.2; 1.3 preferred |
| Client certificates | Out of scope |

Self-signed certificates are the right default for a local tool. The UI must show the fingerprint and
explain that clients need to trust it — an unexplained TLS failure is the single most frustrating
onboarding experience a mail tool can inflict.

### 6.2 Authentication

| Mechanism | Phase | Notes |
| --- | :--: | --- |
| `PLAIN` | 7 | RFC 4616; primary mechanism |
| `LOGIN` | 7 | Legacy, widely required |
| `CRAM-MD5` | ○ | Not implemented; would require a recoverable secret rather than an Argon2 hash |
| `XOAUTH2` | ✗ | Meaningless without a real identity provider |
| `SCRAM-*` | ○ | Deferred |

**Default posture:** authentication is **disabled by default** and Sandesh accepts any credentials.
Sandesh binds to loopback and exists to reduce setup friction; demanding credentials on first run
would contradict the product's purpose. Enabling auth is a single config flag, and CI/shared setups
should. This is a considered trade-off, recorded as ADR-005, and the UI states the current posture
plainly on the dashboard.

CRAM-MD5 requires storing a secret that is recoverable rather than an Argon2 hash, so it is not
implemented: `sandesh-proto`'s SASL layer offers `PLAIN` and `LOGIN` only, on every protocol.

## 7. Deliberately out of scope

| Feature | Reason |
| --- | --- |
| Outbound relay / MTA behaviour | Sandesh is a terminal destination. Accidentally relaying real mail is the worst failure a dev mail tool can have |
| Spam filtering | No value in a development environment |
| DKIM/SPF/DMARC **verification** | *Display and inspection* of these headers is in scope (Phase 8); enforcement is not |
| Sieve (RFC 5228) | Interesting, unjustified for v1 |
| ManageSieve, ACL, QUOTA, multi-tenancy | Out of scope for a local developer tool |
| NNTP, JMAP | JMAP is worth revisiting post-v1; not now |
