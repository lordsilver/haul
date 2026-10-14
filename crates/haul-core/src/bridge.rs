//! A local SOCKS5 endpoint without authentication that forwards every
//! connection through the configured upstream proxy.
//!
//! Browser engines cannot pass proxy credentials, so the desktop app points
//! its in-app browser here; pages a person has to open then leave through the
//! same proxy as the downloads.

use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, RwLock},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tokio_socks::tcp::Socks5Stream;

#[derive(Debug)]
enum Kind {
    Socks5,
    Http,
}

#[derive(Debug)]
struct Upstream {
    kind: Kind,
    host: String,
    port: u16,
    auth: Option<(String, String)>,
}

impl Upstream {
    fn parse(proxy: &str) -> io::Result<Self> {
        let url =
            url::Url::parse(proxy).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let kind = match url.scheme() {
            "socks5" | "socks5h" => Kind::Socks5,
            "http" => Kind::Http,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("cannot bridge a {other} proxy"),
                ));
            }
        };
        let host = url
            .host_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "proxy has no host"))?;
        let port = url.port().unwrap_or(match kind {
            Kind::Socks5 => 1080,
            Kind::Http => 8080,
        });
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let auth = (!url.username().is_empty()).then(|| {
            (
                decode(url.username()),
                decode(url.password().unwrap_or_default()),
            )
        });
        Ok(Self {
            kind,
            host: host.trim_matches(['[', ']']).to_string(),
            port,
            auth,
        })
    }
}

/// The running bridge. Its port stays the same for its whole life (browser
/// profiles remember it), while the proxy behind it can change.
pub struct Bridge {
    addr: SocketAddr,
    upstream: Arc<RwLock<Option<Arc<Upstream>>>>,
}

impl Bridge {
    /// Listens on an ephemeral loopback port, forwarding nowhere until
    /// [`Bridge::set_upstream`]. Must run inside a Tokio runtime.
    pub fn start() -> io::Result<Self> {
        let std_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let listener = TcpListener::from_std(std_listener)?;
        let upstream: Arc<RwLock<Option<Arc<Upstream>>>> = Arc::new(RwLock::new(None));
        let current = upstream.clone();
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    continue;
                };
                let up = current.read().unwrap_or_else(|e| e.into_inner()).clone();
                // Without a proxy there is nowhere to go: drop the connection.
                let Some(up) = up else { continue };
                tokio::spawn(async move {
                    if let Err(e) = serve(client, &up).await {
                        tracing::debug!("bridge connection: {e}");
                    }
                });
            }
        });
        Ok(Self { addr, upstream })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Sends new connections through `proxy`, or nowhere.
    pub fn set_upstream(&self, proxy: Option<&str>) -> io::Result<()> {
        let up = proxy.map(Upstream::parse).transpose()?.map(Arc::new);
        *self.upstream.write().unwrap_or_else(|e| e.into_inner()) = up;
        Ok(())
    }
}

const REPLY_OK: [u8; 10] = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0];

fn reply(code: u8) -> [u8; 10] {
    [5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

async fn serve(mut client: TcpStream, upstream: &Upstream) -> io::Result<()> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not SOCKS5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        client.write_all(&[5, 0xff]).await?;
        return Ok(());
    }
    client.write_all(&[5, 0]).await?;

    let mut req = [0u8; 4];
    client.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            client.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            client.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            client.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => {
            client.write_all(&reply(8)).await?;
            return Ok(());
        }
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    if req[1] != 1 {
        // Only CONNECT; browsers need nothing else.
        client.write_all(&reply(7)).await?;
        return Ok(());
    }
    let mut remote = match connect(upstream, &host, port).await {
        Ok(s) => s,
        Err(e) => {
            client.write_all(&reply(5)).await?;
            return Err(e);
        }
    };
    client.write_all(&REPLY_OK).await?;
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
    Ok(())
}

async fn connect(up: &Upstream, host: &str, port: u16) -> io::Result<TcpStream> {
    match up.kind {
        Kind::Socks5 => {
            let proxy = (up.host.as_str(), up.port);
            let target = (host, port);
            let stream = match &up.auth {
                Some((user, pass)) => {
                    Socks5Stream::connect_with_password(proxy, target, user, pass).await
                }
                None => Socks5Stream::connect(proxy, target).await,
            };
            stream
                .map(Socks5Stream::into_inner)
                .map_err(io::Error::other)
        }
        Kind::Http => {
            let mut s = TcpStream::connect((up.host.as_str(), up.port)).await?;
            let target = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
            if let Some((user, pass)) = &up.auth {
                req.push_str(&format!(
                    "Proxy-Authorization: Basic {}\r\n",
                    STANDARD.encode(format!("{user}:{pass}"))
                ));
            }
            req.push_str("\r\n");
            s.write_all(req.as_bytes()).await?;
            let mut reader = BufReader::new(s);
            let mut status = String::new();
            reader.read_line(&mut status).await?;
            if status.split_whitespace().nth(1) != Some("200") {
                return Err(io::Error::other(format!(
                    "proxy refused CONNECT: {}",
                    status.trim()
                )));
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await? == 0 || line == "\r\n" || line == "\n" {
                    break;
                }
            }
            // A proxy speaks only after the client does, so nothing is buffered.
            Ok(reader.into_inner())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SOCKS5 server that insists on user/pass and then connects for real.
    async fn upstream_with_auth(user: &'static str, pass: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut c, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut head = [0u8; 2];
                    c.read_exact(&mut head).await.unwrap();
                    let mut methods = vec![0u8; head[1] as usize];
                    c.read_exact(&mut methods).await.unwrap();
                    assert!(methods.contains(&2), "client offers user/pass");
                    c.write_all(&[5, 2]).await.unwrap();
                    let mut ver_ulen = [0u8; 2];
                    c.read_exact(&mut ver_ulen).await.unwrap();
                    let mut u = vec![0u8; ver_ulen[1] as usize];
                    c.read_exact(&mut u).await.unwrap();
                    let mut plen = [0u8; 1];
                    c.read_exact(&mut plen).await.unwrap();
                    let mut p = vec![0u8; plen[0] as usize];
                    c.read_exact(&mut p).await.unwrap();
                    let ok = u == user.as_bytes() && p == pass.as_bytes();
                    c.write_all(&[1, if ok { 0 } else { 1 }]).await.unwrap();
                    if !ok {
                        return;
                    }
                    let mut req = [0u8; 4];
                    c.read_exact(&mut req).await.unwrap();
                    assert_eq!(req[3], 3, "the bridge forwards host names, not addresses");
                    let mut len = [0u8; 1];
                    c.read_exact(&mut len).await.unwrap();
                    let mut name = vec![0u8; len[0] as usize];
                    c.read_exact(&mut name).await.unwrap();
                    let mut port = [0u8; 2];
                    c.read_exact(&mut port).await.unwrap();
                    let host = String::from_utf8(name).unwrap();
                    let host = if host == "echo.test" {
                        "127.0.0.1".to_string()
                    } else {
                        host
                    };
                    let mut remote = TcpStream::connect((host.as_str(), u16::from_be_bytes(port)))
                        .await
                        .unwrap();
                    c.write_all(&REPLY_OK).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut c, &mut remote).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn forwards_through_an_authenticated_socks5_proxy() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });
        let up = upstream_with_auth("me", "p@ss").await;
        let bridge = Bridge::start().unwrap();
        assert!(
            Socks5Stream::connect(bridge.addr(), ("echo.test", echo_port))
                .await
                .is_err(),
            "no proxy yet, so nothing goes through"
        );
        bridge
            .set_upstream(Some(&format!("socks5://me:p%40ss@{up}")))
            .unwrap();

        let mut s = Socks5Stream::connect(bridge.addr(), ("echo.test", echo_port))
            .await
            .unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
    }

    #[test]
    fn parses_proxies() {
        let u = Upstream::parse("socks5h://a%20b:c@proxy.example:1081").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("proxy.example", 1081));
        assert_eq!(u.auth, Some(("a b".into(), "c".into())));
        assert!(
            Upstream::parse("http://proxy.example")
                .unwrap()
                .auth
                .is_none()
        );
        assert!(Upstream::parse("socks4://proxy.example:1080").is_err());
    }
}
