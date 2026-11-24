# Peers and invites: "GradApps on this network"

Other GradApps on the LAN, and nudging them into your mesh. Newest first.
Same format as the root `DEVLOG.md`.

---

## 2026-11-25 — presence, invites, and the invite dialog (2.0.1)

### Why
Field report from the owner (2.0.0 on his Windows laptop): Discover only
listed *meshes*, so he saw the one he had joined and nothing else, although
the LAN had tens of devices. He asked for the reverse direction: see every
machine that runs GradApp, pick some, and send an invite that shows up on
theirs as "this device is calling me to contribute to the mesh they created".

### What
**Presence (gm-net `peers`, UDP).** Every running GradApp 2 announces itself
every 3 s, idle or not, on the beacon's port and group (UDP 47800,
239.255.71.77 on every real interface, plus 255.255.255.255 and each directed
broadcast):

```json
{"magic":"gradapp-peer","v":1,"device_id":"3f2a…","name":"MSI-Katana","os":"windows",
 "gpu":"NVIDIA GeForce RTX 3050 Laptop GPU","backend":"cuda","port_state":"ready",
 "status":"hosting","mesh_id":"001f4f1e024c","mesh_name":"MSI-Katana's mesh",
 "app_version":"2.0.1","invite_port":47801}
```

- `{"magic":"gradapp-peer-query"}` makes everyone answer at once; each app
  sends one on start, so the list fills in one round trip, not 3 s.
- Listeners dedupe by `device_id` (random, made once, kept in
  `<config>/GradApp/device-id`), ignore their own packets, forget a peer after
  10 s of silence, and at once on its `"bye":true` (sent on quit and when the
  person hides the app). The receiver records the datagram's source address;
  an `address` field in the packet is ignored. Names are cleaned (no control
  characters, length caps).
- **2.0.0 compatibility:** its beacon listener parses every datagram on 47800
  as a `BeaconPacket`; a presence packet lacks `mesh_id`/`coordinator_port`, so
  serde rejects it, and its query check compares exact bytes with
  `gradmesh-query`. Both are covered by tests (`old_listeners_ignore_presence`).
- `PeerNode` binds the UDP listener *before* spawning its task, so nothing sent
  right after start is lost (a real bug, caught by the expiry test).

**Invites (gm-net `peers`, HTTP).** Each app runs a tiny axum endpoint on TCP
47801 (any free port if taken; advertised as `invite_port`, 0 if none):

- `POST /v1/invite` with `MeshInvite {from_device, from_name, mesh_name,
  mesh_id, coordinator, token, sent_at, nonce, reply_port}`.
- `POST /v1/invite-reply` with `{nonce, from_device, outcome}`.

**gm-types.** `PeerInfo`, `PeerStatus`, `MeshInvite`, `InviteOutcome`;
`AppEvent::PeerSeen`, `PeerGone`, `InviteReceived`, `InviteResult` (additive).

**App (`app/src-tauri/src/peers.rs`).** Starts the node at launch. A 1 s loop
keeps the announcement in step with the app (name from Settings or the
hostname, idle/contributing/hosting, mesh, GPU and backend from the probe,
port state every 15 s) and with *Settings › Visible to other GradApps on this
network* and the blocked list. Commands: `peers_state`, `peer_invite` (many at
once, in parallel), `invite_respond` (`join` / `decline` / `block`),
`peers_unblock_all`. Quit says "bye". Settings gained `visible_on_lan`
(default on) and `blocked_peers` (owned by the backend, like `last_invite`).

**Window.**
- Discover › **GradApps on this network**: a card per peer (name, OS, address,
  version, vendor badge and GPU, status: Idle / Contributing to X / Hosting X,
  port state). Hosting: *Invite to <mesh>* per card, checkboxes, *Select all*,
  *Invite selected (n)* / *Invite all (n)*. Each card then shows what happened:
  *Invite sent · waiting*, *Accepted* (and an *In your mesh* tick), *Not now*,
  *No answer*, *Not delivered* (with why). Not hosting: *Host a mesh to invite
  them*.
- Beside the list, honestly: "Not seeing a machine? It needs GradApp 2 open, on
  this same network, with Visible on. Phones, TVs and machines without GradApp
  cannot receive a nudge." plus the invite link (copy) and its QR code.
- Notices when presence cannot listen (UDP 47800 taken/blocked), when this
  computer is hidden, or when it cannot receive invites.
- **Incoming invite**: a v5 dialog on any screen: "MSI-Katana invites you — to
  contribute to the mesh it created, MSI-Katana's mesh", from/address, mesh,
  machines online (from the coordinator's `/health`), coordinator, a warning
  if this computer is contributing elsewhere, and **[Block this device]
  [Not now] [Join]**. Escape = Not now. If it times out while open it closes
  with a toast. When the window is not in front: an OS notification
  ("MSI-Katana invites you to contribute — … to MSI-Katana's mesh. Open
  GradApp to answer.", only if Settings › Notifications is on) and the taskbar
  entry asks for attention; the dialog is already waiting whenever the window
  is shown.
- Settings › This computer: the visibility toggle; *Blocked devices · Unblock
  all* when any are blocked.
- Mock backend: six fake peers arrive over the first seconds (MSI-Katana,
  studio-m3max contributing, lab-rtx3090 hosting, an Arc tower, a laptop
  installing its port, a NUC that cannot receive invites); invites get scripted
  answers; `?invite=1` (or `?invite=<ms>`) makes MSI-Katana invite this app
  after ~3 s; `?mesh=<name>` names the mesh in `?mock=hosting`.

### Trust model
Same boundary as the rest of GradApp: **being on the LAN**.
- Presence carries no secret. Never the mesh token. A mesh id, a name and a
  GPU model are not secrets (the beacon and `/health` already publish them).
- An invite carries the token, over plain HTTP on the LAN. That is no more
  exposure than today: the host's join page on :3000 serves the same token to
  anyone who can reach it. It is sent only to the device the host picked.
- The receiver accepts an invite only if **all** hold, else it is dropped
  with a status the inviter sees:
  - the TCP peer is a private (RFC 1918), link-local or loopback address, or
    one of this machine's own addresses (403 otherwise);
  - this app is visible (403);
  - well-formed, ≤ 8 KB, ids of the right shape, and the coordinator URL is a
    private/own address or a `.local` name (400) — so an invite can never point
    a machine at a public server;
  - the sender is not blocked (403);
  - at most one invite per sender address per 30 s (429) and at most 5
    waiting (429); a repeated nonce is idempotent;
  - **the coordinator answers `/health`** from here within 2.5 s, with our
    protocol (5) and the mesh id the invite names (422). Unreachable or
    mismatched invites are never shown.
- **Nothing joins by itself.** Join runs the normal join flow (parse →
  `/health` → token → port → worker); the inviter hears "accepted" only after
  that succeeded. Not now and Block reply "declined" (a block is not revealed).
- Replies are accepted only for a nonce this app issued (128-bit random) to
  that device id; otherwise 404. If a reply is lost, the invitee's next
  presence (now in our mesh) also counts as accepted. Unanswered invites time
  out after 2 min on both sides.
- Known limits, accepted for a LAN tool: `device_id` is self-asserted, so
  someone on the LAN can make up a new id after being blocked (the per-address
  rate limit still holds); a LAN attacker can spoof presence (it can only
  *list* a machine; an invite from it still has to point at a working
  coordinator on the LAN, and the person still has to press Join).

### Firewall (Windows) and degrading
- GradApp now listens at start-up (UDP 47800 for presence, TCP 47801 for
  invites), so **Windows Defender Firewall asks on first launch** ("Allow
  GradApp to communicate on these networks"), not only when hosting. Allow it
  on **Private networks**, and make sure the Wi-Fi itself is set to *Private*
  in Windows settings (on a *Public* network Windows blocks inbound traffic
  for every app).
- If the prompt was cancelled (or the network is Public): this machine still
  announces itself (outbound), but it does not hear the others (the list stays
  empty), and invites to it fail on the sender's side with "Could not reach X.
  Its firewall may be blocking GradApp". Fix: Windows Security › Firewall &
  network protection › Allow an app through firewall › GradApp › Private.
- If UDP 47800 cannot be bound, the panel says so and mesh discovery keeps
  working through mDNS and the sweep; if no TCP port can be bound,
  `invite_port` is 0 and other apps show "Cannot receive invites · send the
  link". The rest of GradApp is unaffected either way.
- Hosting still needs the coordinator (python.exe of the port) and :3000
  allowed, as before; an invitee that cannot reach the coordinator answers the
  invite with 422 and the inviter sees "the invited machine could not reach
  your coordinator … check this computer's firewall".

### Verified
- `cargo test -p gm-net`: 8 new unit tests (encode/parse, old listeners ignore
  presence, cleaning and spoofed `address`, private/own address rules,
  table dedupe/expiry/sort, invite checks, the endpoint's decision for a
  public source without a socket) and 8 new integration tests
  (`tests/peers.rs`): two in-process apps see each other in < 1 s, B hears A
  start hosting, A invites B, B gets the vetted invite (`nodes_active` from
  `/health`, sender address from the socket), accepts, A hears "accepted", a
  second invite is rate-limited, B quits and A drops it at once ("bye");
  accepted-by-presence when the reply is lost; the query answered in < 500 ms;
  expiry after the TTL and immediate "bye"; the endpoint's rules (malformed,
  413 oversized, public coordinator, idempotent nonce, rate limit,
  unreachable / wrong protocol / wrong mesh → 422, pending cap, blocked,
  hidden, reply with an unknown nonce); invites timing out on both sides; a
  hidden app saying bye, staying silent and still listing others; presence over
  real multicast/broadcast (skips without a LAN interface / on macOS CI).
  Stable over 5 consecutive runs.
- **Real app, two instances on one machine** under Xvfb (isolated
  `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, separate D-Bus sessions so the
  single-instance plugin keeps them apart; names set in Settings): A got
  invite port 47801, B fell back to an ephemeral port; each listed the other
  within seconds over multicast. A hosted (real coordinator from the test port
  on 8100), invited B; B showed the dialog with "0 online now" from the real
  `/health`; Join ran the real join (port install, worker spawned); A's card
  turned *Contributing to MSI-Katana's mesh · In your mesh*. Quitting B from
  its palette removed it from A's list within 2.5 s; quitting A left no
  coordinator running. Screenshots: `docs/screenshots/v201-peers-list.png`,
  `v201-peers-invite-sent.png`, `v201-peers-invite-received.png`,
  `v201-peers-accepted.png`.
- The container's only adapter is 192.0.2.2 (TEST-NET, not RFC 1918). The
  first live run would have refused the invite as "not from this network";
  that is why "one of this machine's own addresses" counts as trusted (a
  GradApp on the same computer is as trusted as loopback). No bypass switch
  was added.
- Found live, fixed: a third GradApp happened to be running on the same
  machine (another test instance); it started hosting and jumped to the top of
  the list between aiming and clicking, so the click invited the wrong
  machine. Cards are now sorted by name only and never move when a status
  changes. (That instance answered "Not now".)
- Mock in Chrome (puppeteer): list, multi-select, scripted answers
  (accepted → In your mesh, declined, pending), the dialog with `?invite=1`;
  no console errors.

### Not verified
- Two physical machines on a real Wi-Fi, the Windows firewall prompt and its
  "Cancel" path, Windows toast click behaviour (a click activates the app on
  most setups; the dialog is waiting when the window shows), macOS
  notification permission. All on the hardware checklist.

### Next
- A tray item "Invite from X waiting" while the window is hidden.
- Let the installer add the firewall rule for Private networks (NSIS hook), so
  no prompt is needed.
