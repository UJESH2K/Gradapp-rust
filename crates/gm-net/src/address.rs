//! Turning what a person pastes into a target, and the reverse (invite links).
//! Exact port of v1 `network.js::parseAddress` and `main.js`'s invite builder.

use gm_types::{Invite, COORDINATOR_PORT, WEB_PORT};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use url::Url;

/// The URL scheme the desktop app registers (v1 `PROTOCOL`).
pub const INVITE_SCHEME: &str = "gradapp";

/// Ports `parse_address` fills in when the text does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressDefaults {
    pub port: u16,
    pub web_port: u16,
}

impl Default for AddressDefaults {
    fn default() -> Self {
        Self { port: COORDINATOR_PORT, web_port: WEB_PORT }
    }
}

/// `parse_address_with` using the standard ports (8000 / 3000).
pub fn parse_address(text: &str) -> Option<Invite> {
    parse_address_with(text, AddressDefaults::default())
}

/// Turn whatever a person typed into a target: "192.168.1.20", "gradmesh.local",
/// "http://192.168.1.20:3000" (the dashboard address GradMesh prints),
/// "http://192.168.1.20:8000" (the coordinator), or a gradapp:// invite link.
///
/// A port equal to the web port means "the dashboard", so the coordinator is
/// put on the default coordinator port; any other explicit port is the
/// coordinator's. Uses WHATWG URL parsing like v1's `new URL`.
pub fn parse_address_with(text: &str, defaults: AddressDefaults) -> Option<Invite> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if starts_with_ignore_case(text, "gradapp:") {
        // v1: text.replace(/^gradapp:\/*/i, "gradapp://")
        let rest = text["gradapp:".len()..].trim_start_matches('/');
        let url = Url::parse(&format!("gradapp://{rest}")).ok()?;
        let param = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
        let server = param("server").filter(|s| !s.is_empty())?;
        let parsed = parse_address_with(&server, defaults)?;
        return Some(Invite {
            token: param("token").filter(|t| !t.is_empty()),
            mesh_name: param("name"),
            ..parsed
        });
    }
    let candidate = if has_scheme(text) { text.to_string() } else { format!("http://{text}") };
    let url = Url::parse(&candidate).ok()?;
    let hostname = url.host_str().unwrap_or("").to_string();
    let given = url.port().filter(|p| *p != 0);
    let coordinator_port = match given {
        Some(p) if p != defaults.web_port => p,
        _ => defaults.port,
    };
    Some(Invite {
        coordinator: format!("http://{hostname}:{coordinator_port}"),
        web: format!("http://{hostname}:{}", defaults.web_port),
        hostname,
        token: None,
        mesh_name: None,
    })
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// v1: /^[a-z]+:\/\//i
fn has_scheme(text: &str) -> bool {
    let letters = text.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    letters > 0 && text[letters..].starts_with("://")
}

/// JavaScript's `encodeURIComponent` set: everything but A-Z a-z 0-9 - _ . ! ~ * ' ( ).
const URI_COMPONENT: &AsciiSet =
    &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

/// `encodeURIComponent`.
pub fn encode_uri_component(value: &str) -> String {
    utf8_percent_encode(value, URI_COMPONENT).to_string()
}

/// `gradapp://join?server=…&token=…&name=…`, byte-identical to v1's main.js
/// (name defaults to "GradMesh").
pub fn invite_link(coordinator: &str, token: &str, mesh_name: Option<&str>) -> String {
    format!(
        "{INVITE_SCHEME}://join?server={}&token={}&name={}",
        encode_uri_component(coordinator),
        encode_uri_component(token),
        encode_uri_component(mesh_name.filter(|n| !n.is_empty()).unwrap_or("GradMesh")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Option<(String, String, String)> {
        parse_address(text).map(|i| (i.hostname, i.coordinator, i.web))
    }

    fn triple(h: &str, c: &str, w: &str) -> Option<(String, String, String)> {
        Some((h.into(), c.into(), w.into()))
    }

    #[test]
    fn plain_ip_and_hostnames() {
        assert_eq!(p("192.168.1.20"), triple("192.168.1.20", "http://192.168.1.20:8000", "http://192.168.1.20:3000"));
        assert_eq!(p("  192.168.1.20 \n"), triple("192.168.1.20", "http://192.168.1.20:8000", "http://192.168.1.20:3000"));
        assert_eq!(p("gradmesh.local"), triple("gradmesh.local", "http://gradmesh.local:8000", "http://gradmesh.local:3000"));
        // WHATWG lowercases hostnames of special schemes.
        assert_eq!(p("GradMesh.Local"), triple("gradmesh.local", "http://gradmesh.local:8000", "http://gradmesh.local:3000"));
        assert_eq!(p("localhost"), triple("localhost", "http://localhost:8000", "http://localhost:3000"));
    }

    #[test]
    fn ports() {
        // Dashboard port → coordinator on the default port.
        assert_eq!(p("http://192.168.1.20:3000"), triple("192.168.1.20", "http://192.168.1.20:8000", "http://192.168.1.20:3000"));
        assert_eq!(p("192.168.1.20:3000"), triple("192.168.1.20", "http://192.168.1.20:8000", "http://192.168.1.20:3000"));
        assert_eq!(p("http://192.168.1.20:8000"), triple("192.168.1.20", "http://192.168.1.20:8000", "http://192.168.1.20:3000"));
        // Any other port is the coordinator's.
        assert_eq!(p("192.168.1.20:9000"), triple("192.168.1.20", "http://192.168.1.20:9000", "http://192.168.1.20:3000"));
        // :80 is http's default, so WHATWG drops it → default port.
        assert_eq!(p("http://10.0.0.2:80"), triple("10.0.0.2", "http://10.0.0.2:8000", "http://10.0.0.2:3000"));
        // https default 443 dropped too; a path is ignored.
        assert_eq!(p("https://mesh.example:443/join?x=1"), triple("mesh.example", "http://mesh.example:8000", "http://mesh.example:3000"));
        assert_eq!(p("https://mesh.example:8443/"), triple("mesh.example", "http://mesh.example:8443", "http://mesh.example:3000"));
        let custom = parse_address_with("h:3100", AddressDefaults { port: 8100, web_port: 3100 }).unwrap();
        assert_eq!(custom.coordinator, "http://h:8100");
        assert_eq!(custom.web, "http://h:3100");
    }

    #[test]
    fn ipv6_and_rejects() {
        assert_eq!(p("[::1]:8000"), triple("[::1]", "http://[::1]:8000", "http://[::1]:3000"));
        assert_eq!(p(""), None);
        assert_eq!(p("   "), None);
        assert_eq!(p("host:notaport"), None);
        assert_eq!(p("host:99999"), None);
        assert_eq!(p("http://"), None);
        assert_eq!(p("has space.local"), None);
    }

    #[test]
    fn gradapp_links() {
        let link = "gradapp://join?server=http%3A%2F%2F192.168.1.20%3A8000&token=abc%2B1&name=Lab%20Mesh";
        let i = parse_address(link).unwrap();
        assert_eq!(i.hostname, "192.168.1.20");
        assert_eq!(i.coordinator, "http://192.168.1.20:8000");
        assert_eq!(i.web, "http://192.168.1.20:3000");
        assert_eq!(i.token.as_deref(), Some("abc+1"));
        assert_eq!(i.mesh_name.as_deref(), Some("Lab Mesh"));

        // Any number of slashes, any case; '+' in a query is a space.
        let i = parse_address("GRADAPP:join?server=10.0.0.5:3000&name=A+B").unwrap();
        assert_eq!(i.coordinator, "http://10.0.0.5:8000");
        assert_eq!(i.token, None);
        assert_eq!(i.mesh_name.as_deref(), Some("A B"));
        let i = parse_address("gradapp:///join?server=gradmesh.local&token=").unwrap();
        assert_eq!(i.hostname, "gradmesh.local");
        assert_eq!(i.token, None, "empty token → null");
        assert_eq!(i.mesh_name, None, "missing name → null");

        assert_eq!(parse_address("gradapp://join?token=x"), None);
        assert_eq!(parse_address("gradapp://join?server=&token=x"), None);
        assert_eq!(parse_address("gradapp://join?server=bad%20host&token=x"), None);
    }

    #[test]
    fn invite_link_roundtrip() {
        let link = invite_link("http://192.168.1.20:8000", "t0k/+=", Some("Lab's mesh (A)"));
        assert_eq!(
            link,
            "gradapp://join?server=http%3A%2F%2F192.168.1.20%3A8000&token=t0k%2F%2B%3D&name=Lab's%20mesh%20(A)"
        );
        let back = parse_address(&link).unwrap();
        assert_eq!(back.coordinator, "http://192.168.1.20:8000");
        assert_eq!(back.token.as_deref(), Some("t0k/+="));
        assert_eq!(back.mesh_name.as_deref(), Some("Lab's mesh (A)"));
        assert!(invite_link("http://h:8000", "t", None).ends_with("&name=GradMesh"));
    }
}
