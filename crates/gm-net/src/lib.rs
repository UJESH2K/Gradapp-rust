//! gm-net: all of GradApp's networking.
//!
//! * [`lan`]: this machine's LAN addresses (v1 `lanAddresses`) and sweep targets.
//! * [`address`]: `parse_address` (v1 `parseAddress`) and `invite_link`.
//! * [`invite`]: `/health` probes and `read_invite` (`/gradapp.json`, `/join.sh`).
//! * [`discovery`]: `discover` — beacon + mDNS + subnet sweep + manual, deduped.
//! * [`beacon`]: the UDP beacon a hosting app multicasts.
//! * [`join`]: the join surface on port 3000 (v1 `beacon.js`), byte-compatible.
//! * [`client`]: the coordinator API (v1 `api.js`), incl. the SSE event stream.
//! * [`download`]: streaming download to `.part` then atomic rename.

pub mod address;
pub mod beacon;
pub mod client;
pub mod discovery;
pub mod download;
mod error;
pub mod invite;
pub mod join;
pub mod lan;

pub use address::{
    encode_uri_component, invite_link, parse_address, parse_address_with, AddressDefaults,
};
pub use beacon::{beacon_packet, Beacon, BeaconConfig};
pub use client::{CoordinatorClient, SseEvent};
pub use discovery::{discover, DiscoverOptions, MeshTable};
pub use download::download;
pub use error::{NetError, Result};
pub use invite::{lan_client, probe_health, read_invite, HealthHit, InviteInfo, InviteKind};
pub use join::{JoinConfig, JoinInfo, JoinSurface, AGENT_FILES};
pub use lan::{lan_addresses, primary_lan_address, LanAddress};
pub use tokio_util::sync::CancellationToken;
