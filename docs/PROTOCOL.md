# The Remu protocol

Version 1. Normative source: [`crates/remu-proto`](../crates/remu-proto). Where this document
and the code disagree, the code is right — but please file that as a bug.

Remu speaks three separate protocols, and it is worth being clear about which is which:

| Protocol | Between | Transport | Encoding |
|---|---|---|---|
| **Signalling** | client ↔ relay | WebSocket | JSON |
| **Control** | peer ↔ peer | WebRTC data channel `remu.ctrl` | JSON |
| **Bulk** | peer ↔ peer | WebRTC data channel `remu.bulk` | binary |

Media travels as a WebRTC video track, encrypted by DTLS-SRTP, and is never described here because
neither the relay nor this document ever sees it.

---

## 1. Roles

A session always has exactly two roles, and mixing them up is the most common way to misread this
document:

- **Host** — the machine being looked at. It shares its screen and receives input.
- **Controller** — the machine doing the looking. It receives video and sends input.

The controller is the one who *initiates*. "I connect to you" means "I control your desk", which is
the AnyDesk convention and the opposite of a screen-sharing call.

---

## 2. Identity

Every connected client gets a **nine-digit desk ID** (`PeerId`), allocated by the relay, in the range
`100000000..=999999999`. It is displayed grouped — `123 456 789` — and parsed leniently, so a user
can type it with spaces, dashes or nothing at all.

It is serialized as a JSON *string*, not a number. It is an identifier, not a quantity, and keeping
it a string stops any future JavaScript client from reformatting or rounding it.

A **session ID** (`SessionId`, a UUID) identifies one connection *attempt*. It exists so that a stale
`bye` or `ice` from an abandoned attempt cannot disturb a newer session with the same peer.

---

## 3. Signalling

The relay is a dumb forwarder. It allocates IDs and copies envelopes between two peers. It holds no
keys, sees no media, and cannot read the unattended password.

### 3.1 Handshake

```
controller                  relay                    host
    |-- register ----------->|
    |<----------- welcome ---|
    |                        |<------------ register -|
    |                        |-- welcome ------------>|
    |                        |                        |
    |-- connect ------------>|-- connect ------------>|
    |<---------- challenge --|<- challenge (nonce) ---|
    |-- offer (sdp, auth) -->|-- offer -------------->|   host verifies auth
    |<--------- answer (sdp)-|<- answer --------------|
    |<===== ice =============|======= ice ===========>|
    |<~~~~~~~ encrypted peer-to-peer media ~~~~~~~~~~>|
```

### 3.2 Client → relay

| Type | Meaning |
|---|---|
| `register` | First message on every connection. Carries `protocol`, optional `preferredId`, `alias`, `token`. |
| `lookup` | Is this desk online, and what is it called? |
| `connect` | Controller asks to start a session. |
| `challenge` | Host answers with a fresh nonce. |
| `offer` | Controller's SDP offer, plus the auth proof if the host asked for one. |
| `answer` | Host's SDP answer. |
| `ice` | An ICE candidate, either direction. |
| `reject` | This attempt is refused; carries a `RejectReason`. |
| `bye` | This session is over. |
| `ping` | Liveness probe. |

### 3.3 Relay → client

The same set, with `to` rewritten to `from` (and `fromAlias` added where the variant carries it),
plus `welcome`, `lookup-result`, `error` and `pong`.

`register` is answered with `welcome { peerId, protocol, server }`. A `preferredId` is honoured only
if it is currently free — that is how a client keeps its ID across a network blip so saved contacts
still reach it.

### 3.4 Rejection reasons

`declined`, `busy`, `peer-offline`, `bad-password`, `capture-failed`, `timeout`, `cancelled`,
`protocol-mismatch`, `other`. An optional free-text `detail` may accompany any of them; the enum is
what code branches on.

### 3.5 Relay error codes

`malformed-message`, `not-registered`, `already-registered`, `unsupported-protocol`, `unauthorized`,
`rate-limited`, `message-too-large`, `id-unavailable`, `peer-offline`.

### 3.6 Limits

Messages over `MAX_SIGNALING_MESSAGE_BYTES` (256 KiB) are dropped and the sender disconnected. A
screen-share SDP runs a few kilobytes, so this never trips on a legitimate message, and it stops a
hostile client from exhausting relay memory.

---

## 4. Unattended access

When a host has an unattended password set, a controller must prove it knows the password before the
host will skip its accept prompt.

```
proof = HMAC-SHA256(key = password, message = "remu/unattended/v1" || nonce)
```

The nonce is 128 random bits, hex-encoded, **issued by the host**, and single-use.

Three properties follow, and they are the entire reason this exchange exists:

- The relay never sees the password, only a proof it cannot invert.
- A proof captured off the wire authenticates nothing else, because the nonce it is bound to is
  already spent.
- Because the *host* issues the nonce rather than the relay, a malicious relay cannot replay a proof
  by re-serving a nonce of its own choosing.

**What this is not.** It is not a PAKE. A host that is successfully impersonated can collect proofs
and mount an offline dictionary attack against a weak password. Use a strong unattended password, or
leave it unset and accept prompts by hand.

> The Electron predecessor sent a bare `sha256(password)` inside the offer. That value was identical
> on every connection, so observing a single offer was equivalent to knowing the password. The
> `connect`/`challenge` round trip exists to close exactly that hole — and it pays for itself
> anyway, since a busy host can now decline before the controller spends anything on ICE gathering.

---

## 5. The control channel — `remu.ctrl`

Ordered, reliable, JSON. One message per send. Tagged by `kind` in kebab-case.

| Kind | Direction | Meaning |
|---|---|---|
| `input` | controller → host | One `InputEvent`. |
| `chat` | either | Text plus a Unix-ms timestamp. |
| `clipboard` | either | Clipboard text. |
| `cursor-pos` | host → controller | Where the host's real cursor is, normalized. |
| `display-list` | host → controller | Available displays and which is active. |
| `switch-display` | controller → host | Share a different display. |
| `permissions` | host → controller | What the host currently allows. |
| `stats` | host → controller | fps, bitrate, RTT, dropped frames. |
| `file-meta` | either | Announces an incoming file. |
| `file-accept` / `file-refuse` | either | Answer to `file-meta`. |
| `file-end` | either | Final chunk sent; carries the SHA-256. |
| `file-cancel` | either | Abandon a transfer. |

`permissions` is sent as soon as the channel opens and again on every change, so the controller can
grey out what will not work instead of having its input silently dropped.

---

## 6. The bulk channel — `remu.bulk`

Ordered, reliable, **binary**. File bytes only.

```
0        1                          17                   25
+--------+--------------------------+--------------------+----------- ...
| version|   transfer id (uuid)     |  sequence (u64 BE) |  payload
+--------+--------------------------+--------------------+----------- ...
```

Payload is at most 16 KiB, so a frame never exceeds 16409 bytes. 16 KiB is the largest message every
SCTP implementation accepts without negotiating fragmentation, browsers included — worth staying
under so a future web client needs no second framing path. Throughput is governed by the send window
in the transfer loop, not by chunk size.

The sequence number is explicit even though the channel is ordered: it makes a truncated or
duplicated write *detectable* rather than silently corrupting the received file.

### Why two channels

The Electron predecessor multiplexed files and input onto one channel. Because the channel is
ordered, a large file head-of-line blocked every mouse move queued behind it and the remote cursor
visibly froze. Two SCTP streams means a transfer can saturate the link while the session stays
responsive.

### Receiving files safely

The sending peer controls the filename, so the receiver treats it as hostile. `sanitize_filename`
reduces it to a single path component and strips control characters, shell-hostile punctuation,
leading dots and Windows reserved device names; a name that reduces to nothing becomes `download`.
Chunks stream straight to a temp file — never accumulated in memory — the SHA-256 from `file-end` is
verified, and only then is the file renamed into place, never over an existing one.

---

## 7. Input events

Pointer positions are normalized to `0.0..=1.0` of the **shared display**, not pixels. The two
machines rarely share a resolution, and normalizing at the source means the host never needs to know
the controller's window size.

Keys are identified by a platform-neutral `KeyCode` enum naming the *physical position* by its
US-QWERTY legend. A Linux controller can therefore drive a Windows host without either knowing the
other's scancode table. Modifiers are left/right distinct: collapsing them loses AltGr on European
layouts, and a host that released the wrong one would strand the other in a held state.

Every event is clamped by the host via `InputEvent::sanitized()` before use. A peer that sends
`x: 1e9` or `NaN` gets a clamped value, not a cursor flung off-screen or an overflowed conversion.

`release-all` has no equivalent in the predecessor, which is why ending a session there while
holding a modifier left it stuck down on the host until a human pressed and released it locally. The
controller sends it on session end and on window focus loss.

---

## 8. Versioning

`PROTOCOL_VERSION` is sent in `register` and echoed in `welcome`. A mismatch is reported at the
handshake with a clear message rather than tolerated into a session that fails halfway through.

Adding an optional field is a compatible change. Adding a message type is compatible for receivers
that ignore unknown types — note that the current Rust client does **not**, it rejects them, so
adding one is a version bump until that changes. Changing a field's meaning or the bulk framing is a
breaking change and must bump the version.
