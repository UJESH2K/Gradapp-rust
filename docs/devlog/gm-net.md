# gm-net development log

All of GradApp 2's networking: LAN addresses, discovery, invites, the join
surface on port 3000, the beacon, the coordinator client and downloads.
Newest first.

---

## 2026-11-24 — gm-net 2.0.0: v1 networking ported to Rust, plus a LAN beacon

### What
- `lan` — `lan_addresses()` / `primary_lan_address()`: port of `network.js`
  (`VIRTUAL_ADAPTER` filter, link-local skipped, ordering real → Wi-Fi/Ethernet
  → 192.168/10/172). Uses `if-addrs` and keeps the netmask. `sweep_targets()`
  sweeps the real subnet when it is /24 or smaller and the /24 around the
  address when it is larger (v1 always assumed /24).
- `address` — `parse_address` / `parse_address_with`: exact port of
  `parseAddress`, WHATWG URL semantics (default ports dropped, hostnames
  lowercased), `gradapp://` links of any slash count/case. `invite_link()`
  builds the same string as v1 `main.js` (`encodeURIComponent`, name defaults
  to "GradMesh").
- `invite` — `probe_health` (lenient `/health`, latency measured),
  `read_invite` (`/gradapp.json`, then `MESH_TOKEN="…"` from `/join.sh`),
  `lan_client()` (pooled reqwest client that never uses the system proxy).
- `discovery::discover(opts, on_event)` — beacon listener, mDNS
  (`gradmesh.local` via mdns-sd and the OS resolver, browse
  `_gradmesh._tcp.local.`), subnet sweep (128 concurrent, 700 ms per host),
  manual addresses; all confirmed with `/health`, deduped by mesh id with v1's
  rule (an IP replaces a name, nothing else replaces), invites read as soon as
  each mesh is found. Emits `AppEvent::MeshFound` (upsert by `mesh_id`) and
  `AppEvent::ScanProgress`. Ports, targets, timeouts and sources are all
  options; cancellable through `opts.cancel` or by dropping the future.
- `beacon::Beacon` — multicasts a `BeaconPacket` every 2 s to
  239.255.71.77:47800 on every real interface (`IP_MULTICAST_IF` per
  interface, TTL 1), plus 255.255.255.255 and each directed broadcast. Answers a
  `{"magic":"gradmesh-query"}` datagram immediately (rate-limited), and the
  discovery listener sends one on start, so a host is found in one round trip
  instead of up to one interval. Stops on drop.
- `join::JoinSurface::serve(addr, cfg)` — axum port of `beacon.js`:
  `/gradapp.json` (same key order), `/join.sh`, `/join.ps1`, `/`, `/join`,
  `/api/health`, `/logo.png`, `/api/agent/<file>`, optional `/health`
  passthrough, 405 for non-GET/HEAD, 503 while there is no token. The scripts
  and page are templates generated from v1's own functions; tests compare them
  byte-for-byte against fixtures rendered by v1 under Node.
- `client::CoordinatorClient` — `X-Mesh-Token` header (coordinator
  `require_mesh_token`), v1's path allowlist and `detail` error rule, typed
  `health()`, `serde_json::Value` for the rest, `rotate_token()` storing the new
  token, `events()` / `follow_events()` for `/events`, which is
  **server-sent events** (`StreamingResponse`, `text/event-stream`,
  `: keepalive` every 15 s), with v1's 1 s → 15 s reconnect backoff.
- `download()` — streams to `<target>.part`, checks Content-Length, fsyncs,
  renames; the `.part` file is removed on error or cancel.

### Why
- **The beacon.** v1 found meshes by sweeping 254 hosts × port 8000. On a
  campus WiFi that is slow (most addresses black-hole, so each costs a full
  timeout), noisy (it looks like a port scan to the network's IDS, and client
  isolation drops much of it anyway), and wrong whenever the network is not a
  /24 (a /22 dorm network puts the host outside the swept range). A multicast
  beacon reaches every listener on the segment in one datagram, finds the mesh
  in well under a second, and does not care about the subnet size. The sweep
  stays as the fallback for v1 hosts and networks that filter multicast, and
  mDNS stays because the Python coordinator already advertises itself.
- **No system proxy for LAN calls.** A corporate/agent HTTP proxy cannot reach
  192.168.x.x and would see the mesh token; LAN traffic goes direct.
- **Trust model unchanged from v1.** On a LAN, being on the network is the
  boundary: `/health` and the beacon are unauthenticated and carry no secret
  (mesh id is not a secret); the join port shows the mesh token to anyone who
  can reach it, exactly as v1 and GradMesh's dashboard do; every coordinator
  call that matters requires `X-Mesh-Token`; rotating the token is the
  revocation story. A spoofed beacon can only make a host *appear* — it is
  confirmed via `/health` before it is shown, and joining still needs a token.
  The join surface serves only the allowlisted agent files (exact name match
  after percent-decoding; no path traversal possible).

### Verified
- `cargo test -p gm-net`: 21 unit + 11 integration tests green;
  `cargo clippy -p gm-net --all-targets -- -D warnings` clean.
- `parse_address` cases checked against v1's `parseAddress` run under Node;
  join scripts/page byte-identical to v1 output (fixtures in `tests/fixtures`).
- Mock coordinator (axum on 127.0.0.1:0) for probe, `read_invite` both kinds,
  dedupe preference (localhost vs 127.0.0.1), sweep progress, client calls,
  401/404/422 detail mapping, SSE replay + reconnect, token rotation.
- Full 254-host sweep against loopback finishes in well under 3 s.
- JoinSurface traversal attempts (`/api/agent/../../etc/passwd`, `%2e%2e%2f`,
  `..%5c`, NUL, invalid UTF-8, non-allowlisted file in the engine dir) → 404,
  sent as raw HTTP so the path is not normalised by a client.
- Beacon round trip: a unicast-to-127.0.0.1 test always runs; in this
  container multicast loopback *does* work, so the query-over-multicast test
  also runs (found < 500 ms with a 60 s beacon interval). It skips itself on a
  machine with no LAN interface.
- Live, ignored test (`live_coordinator`) against the real Python coordinator
  (`uvicorn coordinator.app:app`): health/mesh/runs/datasets/policy/events/
  rotate all OK; mDNS (`_gradmesh._tcp` + `gradmesh.local`) found it in
  ~0.5 s; full default discovery found it via the sweep in ~50 ms.

### Next
- Teach the Python coordinator (or the v2 host) to send the beacon too, so v1
  hosts are found without the sweep.
- `uploadDataset` / `saveArtifact` (multipart upload, artifact download) from
  v1 `api.js` are not ported yet; `download_with` covers the artifact case.
