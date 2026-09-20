# Architecture

## The one decision everything else follows from

Remu owns its media path.

That sentence is the whole design. A remote-desktop client built on a web runtime hands capture,
encoding and congestion control to the browser engine and cannot reach any of them. You get the
encoder Chromium picked, at the bitrate Chromium chose, fed by a capture path you cannot profile.
That is why such apps burn CPU and why remote text looks soft.

Everything below — the crate split, the choice of GUI toolkit, even which data crosses which
channel — exists so that the sequence

```
capture → colour convert → encode → send → receive → decode → present
```

stays inside one process, in one language, with no serialization boundary anywhere in it.

## Crate graph

```
                          ┌──────────────────┐
                          │  remu-proto   │  wire types, auth, settings
                          │  (no deps: no    │  the single source of truth
                          │  async, no I/O)  │
                          └────────┬─────────┘
             ┌─────────────┬───────┼────────┬──────────────┐
             │             │       │        │              │
      ┌──────┴─────┐ ┌─────┴────┐ ┌┴──────┐ ┌──────┴─────┐ │
      │  -relay    │ │ -capture │ │-input │ │  -session  │ │
      │  (server)  │ │  screen  │ │ enigo │ │  webrtc +  │ │
      │            │ │  scap    │ │       │ │  signalling│ │
      └────────────┘ └─────┬────┘ └───┬───┘ └──────┬─────┘ │
                           │          │            │       │
                     ┌─────┴──────┐   │            │       │
                     │  -codec    │   │            │       │
                     │  openh264  │   │            │       │
                     └─────┬──────┘   │            │       │
                           └──────────┴────────────┴───────┘
                                       │
                               ┌───────┴────────┐
                               │  remu-desk  │  egui + wgpu
                               │  the app       │
                               └────────────────┘
```

Two constraints keep this graph honest:

**`remu-proto` depends on nothing.** No async runtime, no platform crates, no I/O. It is the one
definition of the wire format that the relay, the desk app and any future third-party client all
compile against, rather than three copies that drift.

**`remu-session` is transport only.** It does not depend on `remu-codec` or `remu-capture`;
it moves opaque encoded samples. The wiring between "a frame was captured" and "bytes went on the
wire" lives in the app, not buried in the transport. That is also what let all five engine crates be
written in parallel.

## Why egui and not a webview

The session screen is a video surface with a thin toolbar over it. What it needs is to get a decoded
frame onto the GPU 60 times a second and map a click back to a normalized coordinate.

With egui on wgpu, a decoded frame is uploaded as a texture and drawn. With a webview, the frame has
to cross a process and language boundary every time — or you give the webview's own WebRTC stack the
media back, which forfeits the reason for the rewrite.

The cost is real and worth stating: the Svelte UI was rebuilt rather than carried over, and egui
needs deliberate styling to not look like a debug tool.

## Why WebRTC and not a bespoke QUIC protocol

QUIC would be less code and has better congestion control out of the box. WebRTC was chosen for one
thing it provides that is brutal to write yourself: **ICE**.

NAT traversal is the difference between a tool that works on a corporate network and a demo that
works on your desk. STUN gets most peers connected directly; TURN relays the rest. Adopting WebRTC
means adopting a tested implementation of all of it, plus DTLS-SRTP, plus SDP — which keeps the door
open for a browser client later, since the signalling shape is already what a browser expects.

The costs, stated plainly: encoded frames must be packetized into RTP by hand, and webrtc-rs's
congestion control is weaker than Chromium's. Bitrate adaptation is therefore driven from connection
stats in the app rather than trusted to the stack.

## The two data channels

`remu.ctrl` carries input, chat and clipboard. `remu.bulk` carries file bytes. Both are
ordered and reliable, but they are separate SCTP streams.

The predecessor used one channel for everything. Because it was ordered, a file transfer
head-of-line blocked every mouse move behind it and the remote cursor froze. Splitting the streams
means a transfer can saturate the link while the session stays responsive.

## Session lifecycle

```
 idle ──connect──► offering ──challenge──► authenticating ──offer/answer──► negotiating
                                                                                  │
                                                                              ice │
                                                                                  ▼
   closed ◄──bye/failure── connected ◄──────────────────────────────────── ice complete
```

The host decides at `challenge` time whether it will require a password, before the controller
spends anything on ICE gathering. A busy host declines there too.

## What runs where

| | Host | Controller |
|---|---|---|
| Screen capture | ✅ `remu-capture` | |
| H.264 encode | ✅ `remu-codec` | |
| H.264 decode | | ✅ `remu-codec` |
| Input injection | ✅ `remu-input` | |
| Input capture | | ✅ egui event loop |
| Video track | sends | receives |
| Control channel | both | both |

A single running app can be either, and can be a host to one peer while it is a controller of
another — the roles are per-session, not per-install.

## Threading

The GUI runs on the main thread; egui redraws on demand and continuously while a session is live.
The capture-encode loop runs on its own thread so a slow encode never stalls a frame of UI. The
tokio runtime owns the WebSocket and all WebRTC I/O. These three talk over channels, never over
shared locks on the frame path — a decoded frame is moved, not shared.

## Deliberate non-goals

- **AnyDesk wire compatibility.** Remu cannot talk to an AnyDesk client and never will.
- **A hosted relay.** The project ships a relay you run yourself. There is no Remu service.
- **Hardware encoding, for now.** openh264 is the portable baseline and it is honest about what it
  is: software H.264. VideoToolbox / Media Foundation / VAAPI paths belong behind the existing
  `VideoEncoder` trait, which is why that trait exists.
