//! The internet router, for a new public address: hosts limit what one
//! address may fetch (pixeldrain's 6 GB a day, waits per address), and a
//! home line usually gets a new address when the router redials.
//!
//! The router is found on the network by itself: an SSDP search for an
//! Internet Gateway Device, and a FRITZ!Box's known addresses when nothing
//! answers. Haul asks it over UPnP to redial (`ForceTermination`), which
//! needs no login where the router allows it; a FRITZ!Box that does not
//! takes its own login over TR-064. Then Haul waits for the line to be up
//! with another address. A command of the user's own covers any other
//! router.

use std::{net::Ipv4Addr, sync::LazyLock, time::Duration};

use regex::Regex;
use serde::Serialize;

/// How long a redial may take before Haul stops waiting.
const BACK_WITHIN: Duration = Duration::from_secs(120);

/// A router Haul can ask for a new address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Router {
    /// As it names itself, such as "FRITZ!Box 7530".
    pub model: String,
    /// The WAN connection service: its type and control address.
    #[serde(skip)]
    service: String,
    #[serde(skip)]
    control: String,
    /// TR-064's description, when the router has one (a FRITZ!Box does).
    #[serde(skip)]
    tr064: Option<String>,
}

static MODEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<modelName>([^<]*)</modelName>").unwrap());
static SERVICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<service>\s*<serviceType>([^<]*)</serviceType>.*?<controlURL>([^<]*)</controlURL>",
    )
    .unwrap()
});
static LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^location:\s*(\S+)").unwrap());

/// Where a FRITZ!Box describes itself when nothing answers the search.
const FRITZ: [&str; 2] = [
    "http://fritz.box:49000/igddesc.xml",
    "http://192.168.178.1:49000/igddesc.xml",
];

/// The router on this network, when one answers; at `at` when given.
pub async fn find(http: &reqwest::Client, at: &str) -> Option<Router> {
    let places = if at.is_empty() {
        let mut places = search().await;
        places.extend(FRITZ.iter().map(|p| (*p).to_string()));
        places
    } else {
        vec![at.to_string()]
    };
    for place in places {
        if let Some(router) = describe(http, &place).await {
            return Some(router);
        }
    }
    None
}

/// Description addresses of the gateways that answer an SSDP search.
async fn search() -> Vec<String> {
    let Ok(socket) = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        return Vec::new();
    };
    let ask = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
    if socket
        .send_to(ask.as_bytes(), "239.255.255.250:1900")
        .await
        .is_err()
    {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut buf = [0u8; 2048];
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(until, socket.recv_from(&mut buf)).await {
        let answer = String::from_utf8_lossy(&buf[..n]);
        if let Some(c) = LOCATION.captures(&answer) {
            let place = c[1].to_string();
            if !found.contains(&place) {
                found.push(place);
            }
        }
    }
    found
}

/// The router a description at `place` is, when it has a WAN connection.
async fn describe(http: &reqwest::Client, place: &str) -> Option<Router> {
    let text = http
        .get(place)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let base = url::Url::parse(place).ok()?;
    let (service, control) = SERVICE.captures_iter(&text).find_map(|c| {
        let kind = c[1].to_string();
        let wan = kind.contains(":WANIPConnection:") || kind.contains(":WANPPPConnection:");
        wan.then(|| (kind, base.join(c[2].trim()).ok().map(|u| u.to_string())))
    })?;
    let models: Vec<&str> = MODEL
        .captures_iter(&text)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().trim())
        .collect();
    // The shortest name is the box's own ("FRITZ!Box 7530", not
    // "WANDevice - FRITZ!Box 7530").
    let model = models
        .iter()
        .min_by_key(|m| m.len())
        .map_or("Router", |m| m)
        .to_string();
    let tr064 = base.join("/tr64desc.xml").ok().map(|u| u.to_string());
    Some(Router {
        model,
        service,
        control: control?,
        tr064,
    })
}

fn envelope(service: &str, action: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}"/></s:Body></s:Envelope>"#
    )
}

fn field(xml: &str, name: &str) -> Option<String> {
    let start = xml.find(&format!("<{name}>"))? + name.len() + 2;
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim().to_string())
}

impl Router {
    async fn call(&self, http: &reqwest::Client, action: &str) -> Result<String, String> {
        let r = http
            .post(&self.control)
            .header("Content-Type", r#"text/xml; charset="utf-8""#)
            .header("SoapAction", format!("{}#{action}", self.service))
            .body(envelope(&self.service, action))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        if status >= 400 {
            let why = field(&text, "errorDescription").unwrap_or_else(|| format!("HTTP {status}"));
            return Err(why);
        }
        Ok(text)
    }

    /// The address the internet sees, as the router says.
    pub async fn address(&self, http: &reqwest::Client) -> Option<String> {
        let text = self.call(http, "GetExternalIPAddress").await.ok()?;
        field(&text, "NewExternalIPAddress").filter(|a| !a.is_empty() && a != "0.0.0.0")
    }

    async fn connected(&self, http: &reqwest::Client) -> bool {
        self.call(http, "GetStatusInfo")
            .await
            .ok()
            .and_then(|t| field(&t, "NewConnectionStatus"))
            .is_some_and(|s| s == "Connected")
    }

    /// Waits for the line to be up with an address other than `before`,
    /// after something else made it redial.
    pub async fn settled(
        &self,
        http: &reqwest::Client,
        before: Option<String>,
    ) -> Result<String, String> {
        wait_for_new(before, || async {
            (self.connected(http).await, self.address(http).await)
        })
        .await
    }

    /// Redials the line and waits for it to be up with another address:
    /// over UPnP, or, when the router refuses that, over TR-064 with
    /// `login`. Returns the new address.
    pub async fn redial(
        &self,
        http: &reqwest::Client,
        login: Option<(&str, &str)>,
    ) -> Result<String, String> {
        let before = self.address(http).await;
        let asked = match self.call(http, "ForceTermination").await {
            Ok(_) => Ok(()),
            Err(upnp) => match (login, &self.tr064) {
                (Some((user, password)), Some(desc)) => tr064_redial(http, desc, user, password)
                    .await
                    .map_err(|e| format!("{upnp}; TR-064: {e}")),
                _ => Err(format!(
                    "{} refused to redial over UPnP ({upnp}); give Haul its login",
                    self.model
                )),
            },
        };
        asked?;
        wait_for_new(before, || async {
            (self.connected(http).await, self.address(http).await)
        })
        .await
    }
}

/// Waits until the line is up with an address other than `before`.
async fn wait_for_new<F, Fut>(before: Option<String>, mut check: F) -> Result<String, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = (bool, Option<String>)>,
{
    // The line goes down first; asking at once would see the old address.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let until = tokio::time::Instant::now() + BACK_WITHIN;
    while tokio::time::Instant::now() < until {
        if let (true, Some(now)) = check().await {
            return if Some(&now) == before.as_ref() {
                Err(format!(
                    "the line came back with the same address ({now}): this line keeps it"
                ))
            } else {
                Ok(now)
            };
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err("the line did not come back within two minutes".into())
}

/// TR-064's WAN service: PPP (DSL) when the router has it, else IP.
async fn tr064_redial(
    http: &reqwest::Client,
    desc: &str,
    user: &str,
    password: &str,
) -> Result<(), String> {
    let text = http
        .get(desc)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let base = url::Url::parse(desc).map_err(|e| e.to_string())?;
    let services: Vec<(String, String)> = SERVICE
        .captures_iter(&text)
        .map(|c| (c[1].to_string(), c[2].trim().to_string()))
        .collect();
    let (service, control) = services
        .iter()
        .find(|(t, _)| t.contains(":WANPPPConnection:"))
        .or_else(|| {
            services
                .iter()
                .find(|(t, _)| t.contains(":WANIPConnection:"))
        })
        .ok_or("no WAN connection service")?;
    let url = base.join(control).map_err(|e| e.to_string())?;
    let body = envelope(service, "ForceTermination");
    let send = |auth: Option<String>| {
        let mut req = http
            .post(url.clone())
            .header("Content-Type", r#"text/xml; charset="utf-8""#)
            .header("SoapAction", format!("{service}#ForceTermination"))
            .body(body.clone())
            .timeout(Duration::from_secs(10));
        if let Some(auth) = auth {
            req = req.header("Authorization", auth);
        }
        req.send()
    };
    let first = send(None).await.map_err(|e| e.without_url().to_string())?;
    let r = if first.status() == 401 {
        let challenge = first
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .ok_or("no login challenge")?
            .to_string();
        let auth = digest(&challenge, user, password, "POST", url.path())
            .ok_or("an unknown login challenge")?;
        send(Some(auth))
            .await
            .map_err(|e| e.without_url().to_string())?
    } else {
        first
    };
    match r.status().as_u16() {
        401 => Err("the router refused the login".into()),
        // Some routers answer once the line is already going down.
        s if s < 400
            || s == 500
                && r.text()
                    .await
                    .unwrap_or_default()
                    .contains("DisconnectInProgress") =>
        {
            Ok(())
        }
        s => Err(format!("HTTP {s}")),
    }
}

/// An HTTP Digest answer (RFC 7616, MD5, `qop=auth`) to `challenge`.
fn digest(challenge: &str, user: &str, password: &str, method: &str, uri: &str) -> Option<String> {
    use md5::{Digest as _, Md5};
    let part = |name: &str| {
        Regex::new(&format!(r#"(?i)\b{name}="?([^",]+)"?"#))
            .ok()?
            .captures(challenge)
            .map(|c| c[1].to_string())
    };
    let realm = part("realm")?;
    let nonce = part("nonce")?;
    let hex = |s: String| -> String {
        Md5::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let ha1 = hex(format!("{user}:{realm}:{password}"));
    let ha2 = hex(format!("{method}:{uri}"));
    let cnonce = format!("{:016x}", uuid::Uuid::new_v4().as_u128() as u64);
    let nc = "00000001";
    let response = hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
    Some(format!(
        r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", qop=auth, nc={nc}, cnonce="{cnonce}", response="{response}", algorithm=MD5"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_answer_is_rfc_7616s() {
        // RFC 2617's example, with this cnonce.
        let challenge = r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let answer = digest(
            challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
        )
        .unwrap();
        assert!(answer.contains(r#"realm="testrealm@host.com""#), "{answer}");
        assert!(answer.contains(r#"nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#));
        let cnonce = Regex::new(r#"cnonce="([^"]+)""#)
            .unwrap()
            .captures(&answer)
            .unwrap()[1]
            .to_string();
        use md5::{Digest as _, Md5};
        let hex = |s: &str| -> String {
            Md5::digest(s.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        let expected = hex(&format!(
            "{}:dcd98b7102dd2f0e8b11d0f600bfb0c093:00000001:{cnonce}:auth:{}",
            hex("Mufasa:testrealm@host.com:Circle Of Life"),
            hex("GET:/dir/index.html")
        ));
        assert!(answer.contains(&format!(r#"response="{expected}""#)));
        assert_eq!(digest("Basic realm=\"x\"", "u", "p", "GET", "/"), None);
    }

    #[test]
    fn soap_fields_and_descriptions() {
        assert_eq!(
            field(
                "<a><NewExternalIPAddress> 81.1.2.3 </NewExternalIPAddress></a>",
                "NewExternalIPAddress"
            )
            .as_deref(),
            Some("81.1.2.3")
        );
        assert_eq!(field("<a/>", "X"), None);
        let desc = r#"<root><device><modelName>FRITZ!Box 7530</modelName>
            <device><modelName>WANDevice - FRITZ!Box 7530</modelName>
            <service><serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType><controlURL>/igdupnp/control/WANCommonIFC1</controlURL></service>
            <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/igdupnp/control/WANIPConn1</controlURL></service>
            </device></device></root>"#;
        let found: Vec<(String, String)> = SERVICE
            .captures_iter(desc)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].1, "/igdupnp/control/WANIPConn1");
    }

    #[tokio::test]
    async fn a_redial_needs_another_address() {
        let same = wait_for_new(Some("1.1.1.1".into()), || async {
            (true, Some("1.1.1.1".to_string()))
        })
        .await;
        assert!(same.unwrap_err().contains("same address"));
        let new = wait_for_new(Some("1.1.1.1".into()), || async {
            (true, Some("2.2.2.2".to_string()))
        })
        .await;
        assert_eq!(new.unwrap(), "2.2.2.2");
    }
}
