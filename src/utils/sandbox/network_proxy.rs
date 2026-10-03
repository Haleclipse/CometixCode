//! Native network proxy used by the Bash OS sandbox.
//!
//! Maps to: `@anthropic-ai/sandbox-runtime` 0.2.x
//! `sandbox/{sandbox-manager,http-proxy,socks-proxy,linux-sandbox-utils}.ts`,
//! as consumed by CC `utils/sandbox/sandbox-adapter.ts`.
//!
//! HTTP CONNECT and SOCKS5 traffic is checked against the live domain policy.
//! On Linux, host-side `socat` bridges expose the proxies through Unix sockets
//! that can be bind-mounted into bwrap's isolated network namespace.

use super::sandbox_adapter::{NetworkHostPattern, ask_for_network_host_permission};
use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkProxyEndpoints {
    pub http_port: u16,
    pub socks_port: u16,
    pub linux_http_socket: Option<PathBuf>,
    pub linux_socks_socket: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
struct NetworkPolicy {
    allowed_domains: Vec<String>,
    denied_domains: Vec<String>,
}

static NETWORK_POLICY: LazyLock<Mutex<NetworkPolicy>> =
    LazyLock::new(|| Mutex::new(NetworkPolicy::default()));
static NETWORK_RUNTIME: LazyLock<Mutex<Option<Arc<NetworkProxyRuntime>>>> =
    LazyLock::new(|| Mutex::new(None));

struct NetworkProxyRuntime {
    endpoints: NetworkProxyEndpoints,
    external_http_port: Option<u16>,
    external_socks_port: Option<u16>,
    shutdown: Arc<AtomicBool>,
    bridges: Mutex<Vec<Child>>,
}

impl NetworkProxyRuntime {
    fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Ok(mut bridges) = self.bridges.lock() {
            for bridge in bridges.iter_mut() {
                let _ = bridge.kill();
                let _ = bridge.wait();
            }
            bridges.clear();
        }
        for path in [
            self.endpoints.linux_http_socket.as_ref(),
            self.endpoints.linux_socks_socket.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Native runtime boundary for sandbox-manager.ts `updateConfig`: existing
/// proxy listeners read this shared policy on each request. Refresh does not
/// initialize a listener or restart an in-flight shell.
pub(super) fn update_network_config(config: &super::sandbox_adapter::NetworkRestrictionConfig) {
    let mut policy = NETWORK_POLICY.lock().unwrap_or_else(|p| p.into_inner());
    policy.allowed_domains = config.allowed_domains.clone();
    policy.denied_domains = config.denied_domains.clone();
}

/// Start or reuse proxy infrastructure and atomically update its live policy.
pub fn ensure_network_proxy(
    config: &super::sandbox_adapter::NetworkRestrictionConfig,
) -> Result<NetworkProxyEndpoints> {
    update_network_config(config);

    let mut slot = NETWORK_RUNTIME
        .lock()
        .map_err(|_| anyhow!("sandbox network proxy state is poisoned"))?;
    if let Some(runtime) = slot.as_ref() {
        if runtime.external_http_port == config.http_proxy_port
            && runtime.external_socks_port == config.socks_proxy_port
        {
            return Ok(runtime.endpoints.clone());
        }
        runtime.stop();
        *slot = None;
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let http_port = match config.http_proxy_port {
        Some(port) => port,
        None => start_http_proxy(Arc::clone(&shutdown))?,
    };
    let socks_port = match config.socks_proxy_port {
        Some(port) => port,
        None => start_socks_proxy(Arc::clone(&shutdown))?,
    };

    let (linux_http_socket, linux_socks_socket, bridges) = if cfg!(target_os = "linux") {
        let (http_socket, socks_socket, bridges) =
            start_linux_bridges(http_port, socks_port, Arc::clone(&shutdown))?;
        (Some(http_socket), Some(socks_socket), bridges)
    } else {
        (None, None, Vec::new())
    };
    let runtime = Arc::new(NetworkProxyRuntime {
        endpoints: NetworkProxyEndpoints {
            http_port,
            socks_port,
            linux_http_socket,
            linux_socks_socket,
        },
        external_http_port: config.http_proxy_port,
        external_socks_port: config.socks_proxy_port,
        shutdown,
        bridges: Mutex::new(bridges),
    });
    let weak = Arc::downgrade(&runtime);
    crate::utils::cleanup_registry::register_cleanup(move || {
        let weak = weak.clone();
        async move {
            if let Some(runtime) = weak.upgrade() {
                runtime.stop();
            }
        }
    });
    let endpoints = runtime.endpoints.clone();
    *slot = Some(runtime);
    Ok(endpoints)
}

fn start_http_proxy(shutdown: Arc<AtomicBool>) -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("Could not bind the sandbox HTTP proxy")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new()
        .name("cometix-sandbox-http-proxy".to_string())
        .spawn(move || accept_loop(listener, shutdown, handle_http_connection))
        .context("Could not start the sandbox HTTP proxy")?;
    Ok(port)
}

fn start_socks_proxy(shutdown: Arc<AtomicBool>) -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("Could not bind the sandbox SOCKS proxy")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new()
        .name("cometix-sandbox-socks-proxy".to_string())
        .spawn(move || accept_loop(listener, shutdown, handle_socks_connection))
        .context("Could not start the sandbox SOCKS proxy")?;
    Ok(port)
}

fn accept_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    handler: fn(TcpStream) -> Result<()>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                std::thread::spawn(move || {
                    // Darwin accept inherits O_NONBLOCK from the listener.
                    // This worker uses blocking Read/Write (including copy in
                    // tunnel), unlike the source Node stream's readiness loop.
                    // Only accept polling is nonblocking; a payload arriving
                    // later must wait rather than close the upstream on EAGAIN.
                    let result = stream
                        .set_nonblocking(false)
                        .map_err(anyhow::Error::from)
                        .and_then(|()| handler(stream));
                    if let Err(error) = result {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Sandbox network proxy connection failed: {error:#}"
                        ));
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "Sandbox network proxy listener failed: {error}"
                ));
                break;
            }
        }
    }
}

fn read_http_header(stream: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>)> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let mut bytes = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            bail!("HTTP proxy client closed before sending a request");
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_HTTP_HEADER_BYTES {
            bail!("HTTP proxy request headers exceed 64 KiB");
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let body = bytes.split_off(index + 4);
            return Ok((bytes, body));
        }
    }
}

fn handle_http_connection(mut client: TcpStream) -> Result<()> {
    client.set_nodelay(true)?;
    let (header, buffered_body) = read_http_header(&mut client)?;
    let header_text = std::str::from_utf8(&header).context("HTTP proxy headers are not UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = request_parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = request_parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if request_parts.next().is_some() || !version.starts_with("HTTP/") {
        bail!("invalid HTTP proxy request line");
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = parse_authority(target, None)?;
        if !network_request_allowed(&host, port) {
            client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nX-Proxy-Error: blocked-by-allowlist\r\nConnection: close\r\n\r\nConnection blocked by network allowlist")?;
            return Ok(());
        }
        let mut upstream = connect_target(&host, port)?;
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        if !buffered_body.is_empty() {
            upstream.write_all(&buffered_body)?;
        }
        tunnel(client, upstream)?;
        return Ok(());
    }

    let parsed = parse_absolute_http_target(target)?;
    if parsed.scheme != "http" {
        client.write_all(
            b"HTTP/1.1 501 Not Implemented\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )?;
        return Ok(());
    }
    if !network_request_allowed(&parsed.host, parsed.port) {
        client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nX-Proxy-Error: blocked-by-allowlist\r\nConnection: close\r\n\r\nConnection blocked by network allowlist")?;
        return Ok(());
    }

    let mut content_length = 0usize;
    let mut forwarded_headers = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed HTTP proxy header");
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && !value.trim().eq_ignore_ascii_case("identity")
        {
            client.write_all(
                b"HTTP/1.1 501 Not Implemented\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            )?;
            return Ok(());
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .trim()
                .parse::<usize>()
                .context("invalid HTTP Content-Length")?;
        }
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        forwarded_headers.push((name, value.trim()));
    }
    if content_length > 64 * 1024 * 1024 {
        bail!("HTTP proxy request body exceeds 64 MiB");
    }

    let mut upstream = connect_target(&parsed.host, parsed.port)?;
    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;
    write!(
        upstream,
        "{method} {} {version}\r\nHost: {}\r\nConnection: close\r\n",
        parsed.path_and_query, parsed.authority
    )?;
    for (name, value) in forwarded_headers {
        write!(upstream, "{name}: {value}\r\n")?;
    }
    upstream.write_all(b"\r\n")?;
    let buffered = buffered_body.len().min(content_length);
    upstream.write_all(&buffered_body[..buffered])?;
    let mut remaining = content_length - buffered;
    let mut chunk = [0u8; 8192];
    while remaining > 0 {
        let read_len = chunk.len().min(remaining);
        let count = client.read(&mut chunk[..read_len])?;
        if count == 0 {
            bail!("HTTP proxy client closed before its request body completed");
        }
        upstream.write_all(&chunk[..count])?;
        remaining -= count;
    }
    upstream.shutdown(Shutdown::Write)?;
    std::io::copy(&mut upstream, &mut client)?;
    Ok(())
}

struct AbsoluteHttpTarget {
    scheme: String,
    host: String,
    port: u16,
    authority: String,
    path_and_query: String,
}

fn parse_absolute_http_target(target: &str) -> Result<AbsoluteHttpTarget> {
    let (scheme, rest, default_port) = if let Some(rest) = target.strip_prefix("http://") {
        ("http", rest, 80)
    } else if let Some(rest) = target.strip_prefix("https://") {
        ("https", rest, 443)
    } else {
        bail!("HTTP proxy request target is not an absolute URI");
    };
    let boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..boundary];
    if authority.is_empty() || authority.contains('@') {
        bail!("invalid HTTP proxy URI authority");
    }
    let (host, port) = parse_authority(authority, Some(default_port))?;
    let suffix = &rest[boundary..];
    let path_and_query = if suffix.is_empty() {
        "/".to_string()
    } else if suffix.starts_with('/') {
        suffix.split('#').next().unwrap_or("/").to_string()
    } else {
        format!("/{}", suffix.split('#').next().unwrap_or_default())
    };
    Ok(AbsoluteHttpTarget {
        scheme: scheme.to_string(),
        host,
        port,
        authority: authority.to_string(),
        path_and_query,
    })
}

fn parse_authority(authority: &str, default_port: Option<u16>) -> Result<(String, u16)> {
    if authority.is_empty() || authority.contains(['\r', '\n', '\0', '@']) {
        bail!("invalid network authority");
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| anyhow!("invalid IPv6 authority"))?;
        let host = &rest[..end];
        let tail = &rest[end + 1..];
        let port = if let Some(port) = tail.strip_prefix(':') {
            parse_port(port)?
        } else if tail.is_empty() {
            default_port.ok_or_else(|| anyhow!("network authority is missing a port"))?
        } else {
            bail!("invalid IPv6 authority");
        };
        return Ok((host.to_string(), port));
    }
    let colon_count = authority.bytes().filter(|byte| *byte == b':').count();
    if colon_count > 1 {
        bail!("IPv6 network authorities must use brackets");
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() {
            bail!("network authority is missing a host");
        }
        return Ok((host.to_string(), parse_port(port)?));
    }
    Ok((
        authority.to_string(),
        default_port.ok_or_else(|| anyhow!("network authority is missing a port"))?,
    ))
}

fn parse_port(value: &str) -> Result<u16> {
    let port = value.parse::<u16>().context("invalid network port")?;
    if port == 0 {
        bail!("network port must be between 1 and 65535");
    }
    Ok(port)
}

fn handle_socks_connection(mut client: TcpStream) -> Result<()> {
    client.set_read_timeout(Some(IO_TIMEOUT))?;
    client.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting)?;
    if greeting[0] != 5 {
        bail!("unsupported SOCKS version");
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods)?;
    if !methods.contains(&0) {
        client.write_all(&[5, 0xff])?;
        return Ok(());
    }
    client.write_all(&[5, 0])?;

    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    if request[0] != 5 || request[1] != 1 || request[2] != 0 {
        send_socks_status(&mut client, 7)?;
        return Ok(());
    }
    let host = match request[3] {
        1 => {
            let mut address = [0u8; 4];
            client.read_exact(&mut address)?;
            IpAddr::from(address).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            client.read_exact(&mut length)?;
            if length[0] == 0 {
                send_socks_status(&mut client, 8)?;
                return Ok(());
            }
            let mut host = vec![0u8; length[0] as usize];
            client.read_exact(&mut host)?;
            String::from_utf8(host).context("SOCKS domain is not UTF-8")?
        }
        4 => {
            let mut address = [0u8; 16];
            client.read_exact(&mut address)?;
            IpAddr::from(address).to_string()
        }
        _ => {
            send_socks_status(&mut client, 8)?;
            return Ok(());
        }
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    if port == 0 || !network_request_allowed(&host, port) {
        send_socks_status(&mut client, 2)?;
        return Ok(());
    }
    match connect_target(&host, port) {
        Ok(upstream) => {
            send_socks_status(&mut client, 0)?;
            tunnel(client, upstream)?;
        }
        Err(_) => send_socks_status(&mut client, 4)?,
    }
    Ok(())
}

fn send_socks_status(stream: &mut TcpStream, status: u8) -> std::io::Result<()> {
    stream.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0])
}

fn connect_target(host: &str, port: u16) -> Result<TcpStream> {
    let addresses = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("Could not resolve {host}"))?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(IO_TIMEOUT))?;
                stream.set_write_timeout(Some(IO_TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow!("No address resolved for {host}")))
}

fn tunnel(mut client: TcpStream, mut upstream: TcpStream) -> Result<()> {
    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;
    let mut client_reader = client.try_clone()?;
    let mut upstream_writer = upstream.try_clone()?;
    let forward = std::thread::spawn(move || {
        let result = std::io::copy(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
        result
    });
    let reverse = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = forward.join();
    reverse?;
    Ok(())
}

fn network_request_allowed(host: &str, port: u16) -> bool {
    let Some(canonical) = canonicalize_host(host) else {
        return false;
    };
    let policy = NETWORK_POLICY
        .lock()
        .map(|policy| policy.clone())
        .unwrap_or_default();
    if policy
        .denied_domains
        .iter()
        .any(|pattern| matches_domain_pattern(&canonical, pattern))
    {
        return false;
    }
    if policy
        .allowed_domains
        .iter()
        .any(|pattern| matches_domain_pattern(&canonical, pattern))
    {
        return true;
    }
    futures::executor::block_on(ask_for_network_host_permission(
        NetworkHostPattern::with_port(host, port),
    ))
}

fn matches_domain_pattern(host: &str, pattern: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    if let Some(base) = pattern.strip_prefix("*.") {
        if host.parse::<IpAddr>().is_ok() {
            return false;
        }
        return host.ends_with(&format!(".{base}"));
    }
    host == pattern
}

fn canonicalize_host(host: &str) -> Option<String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if bare.is_empty()
        || bare.len() > 255
        || bare.contains('%')
        || !bare
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return None;
    }
    if let Ok(address) = bare.parse::<IpAddr>() {
        return Some(address.to_string());
    }
    if let Some(address) = parse_legacy_ipv4(bare) {
        return Some(address.to_string());
    }
    if bare.contains(':') {
        return None;
    }
    Some(bare.trim_end_matches('.').to_ascii_lowercase())
}

fn parse_legacy_ipv4(host: &str) -> Option<Ipv4Addr> {
    let parts = host.trim_end_matches('.').split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 4 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    let values = parts
        .iter()
        .map(|part| parse_ipv4_number(part))
        .collect::<Option<Vec<_>>>()?;
    let value = match values.as_slice() {
        [a] if *a <= u32::MAX as u64 => *a,
        [a, b] if *a <= 0xff && *b <= 0x00ff_ffff => (a << 24) | b,
        [a, b, c] if *a <= 0xff && *b <= 0xff && *c <= 0xffff => (a << 24) | (b << 16) | c,
        [a, b, c, d] if values.iter().all(|value| *value <= 0xff) => {
            (a << 24) | (b << 16) | (c << 8) | d
        }
        _ => return None,
    };
    Some(Ipv4Addr::from(value as u32))
}

fn parse_ipv4_number(value: &str) -> Option<u64> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return if hex.is_empty() {
            Some(0)
        } else {
            u64::from_str_radix(hex, 16).ok()
        };
    }
    if value.len() > 1 && value.starts_with('0') {
        return u64::from_str_radix(&value[1..], 8).ok();
    }
    value.parse::<u64>().ok()
}

#[cfg(target_os = "linux")]
fn start_linux_bridges(
    http_port: u16,
    socks_port: u16,
    shutdown: Arc<AtomicBool>,
) -> Result<(PathBuf, PathBuf, Vec<Child>)> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    // sandbox-runtime `linux-sandbox-utils.js:326-327` `join(tmpdir(), …)`.
    let http_socket = crate::utils::node_os::tmpdir().join(format!("cometix-http-{token}.sock"));
    let socks_socket = crate::utils::node_os::tmpdir().join(format!("cometix-socks-{token}.sock"));
    let mut bridges = Vec::new();
    for (socket, port) in [(&http_socket, http_port), (&socks_socket, socks_port)] {
        let mut command = Command::new("socat");
        // sandbox-runtime `spawn('socat', ...)` inherits process.env; the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        let child = command
            .arg(format!("UNIX-LISTEN:{},fork,reuseaddr", socket.display()))
            .arg(format!(
                "TCP:localhost:{port},keepalive,keepidle=10,keepintvl=5,keepcnt=3"
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Could not start the Linux sandbox network bridge")?;
        bridges.push(child);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if shutdown.load(Ordering::Acquire) {
            bail!("Sandbox network bridge startup was cancelled");
        }
        let exited = bridges
            .iter_mut()
            .any(|child| child.try_wait().ok().flatten().is_some());
        if exited {
            bail!("Linux sandbox network bridge exited during startup");
        }
        if http_socket.exists() && socks_socket.exists() {
            return Ok((http_socket, socks_socket, bridges));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for child in &mut bridges {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(&http_socket);
    let _ = std::fs::remove_file(&socks_socket);
    bail!("Linux sandbox network bridges did not become ready")
}

#[cfg(not(target_os = "linux"))]
fn start_linux_bridges(
    _http_port: u16,
    _socks_port: u16,
    _shutdown: Arc<AtomicBool>,
) -> Result<(PathBuf, PathBuf, Vec<Child>)> {
    unreachable!("Linux bridges are only started on Linux")
}

#[cfg(test)]
pub fn reset_network_proxy_for_test() {
    if let Ok(mut slot) = NETWORK_RUNTIME.lock() {
        if let Some(runtime) = slot.take() {
            runtime.stop();
        }
    }
    if let Ok(mut policy) = NETWORK_POLICY.lock() {
        *policy = NetworkPolicy::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn canonicalization_blocks_inet_aton_and_wildcard_ip_bypasses() {
        assert_eq!(
            canonicalize_host("2130706433").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            canonicalize_host("0x7f.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(canonicalize_host("127.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(canonicalize_host("0x").as_deref(), Some("0.0.0.0"));
        assert!(!matches_domain_pattern("127.0.0.1", "*.0.0.1"));
        assert!(canonicalize_host("evil.com\0.allowed.com").is_none());
        assert!(canonicalize_host("::ffff:1.2.3.4%x.allowed.com").is_none());
    }

    #[test]
    fn regular_http_proxy_rewrites_absolute_uri_and_relays_one_response() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_network_proxy_for_test();
        super::super::sandbox_adapter::clear_sandbox_ask_callback_for_test();
        let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        let origin_thread = std::thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
            }
            assert!(
                String::from_utf8_lossy(&request).starts_with("GET /path HTTP/1.1\r\n"),
                "{:?}",
                String::from_utf8_lossy(&request)
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let endpoints =
            ensure_network_proxy(&super::super::sandbox_adapter::NetworkRestrictionConfig {
                allowed_domains: vec!["127.0.0.1".to_string()],
                ..Default::default()
            })
            .unwrap();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoints.http_port)).unwrap();
        write!(
            client,
            "GET http://127.0.0.1:{origin_port}/path HTTP/1.1\r\nHost: ignored\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.ends_with("\r\n\r\nok"), "{response:?}");
        origin_thread.join().unwrap();
        reset_network_proxy_for_test();
    }

    #[test]
    fn http_connect_proxy_allows_only_configured_host() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        reset_network_proxy_for_test();
        super::super::sandbox_adapter::clear_sandbox_ask_callback_for_test();
        let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let (mut stream, _) = echo.accept().unwrap();
            let mut bytes = [0u8; 4];
            stream.read_exact(&mut bytes).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let endpoints =
            ensure_network_proxy(&super::super::sandbox_adapter::NetworkRestrictionConfig {
                allowed_domains: vec!["127.0.0.1".to_string()],
                ..Default::default()
            })
            .unwrap();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoints.http_port)).unwrap();
        write!(
            client,
            "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\n\r\n"
        )
        .unwrap();
        let mut response = [0u8; 39];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"HTTP/1.1 200 Connection Established\r\n\r\n");
        client.write_all(b"ping").unwrap();
        let mut echoed = [0u8; 4];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        drop(client);
        echo_thread.join().unwrap();

        let mut denied = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoints.http_port)).unwrap();
        denied
            .write_all(b"CONNECT example.invalid:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        denied.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"));
        reset_network_proxy_for_test();
    }
    #[test]
    fn refresh_live_policy_matches_official_update_config_without_starting_proxy() {
        // sandbox-adapter.ts:798-803 / sandbox-manager.ts updateConfig:
        // policy replacement is synchronous; refreshing alone starts no proxy.
        let config = super::super::sandbox_adapter::NetworkRestrictionConfig {
            allowed_domains: vec!["fresh.example".into()],
            denied_domains: vec!["blocked.example".into()],
            ..Default::default()
        };
        update_network_config(&config);
        let policy = NETWORK_POLICY.lock().unwrap().clone();
        assert_eq!(policy.allowed_domains, config.allowed_domains);
        assert_eq!(policy.denied_domains, config.denied_domains);
        assert!(NETWORK_RUNTIME.lock().unwrap().is_none());
    }
    #[cfg(unix)]
    #[test]
    fn accepted_proxy_stream_is_blocking_before_payload_handling() {
        use std::os::fd::AsRawFd;
        static OBSERVED: std::sync::OnceLock<std::sync::mpsc::Sender<bool>> =
            std::sync::OnceLock::new();
        fn observe(stream: TcpStream) -> Result<()> {
            // F_GETFL reads the accepted socket's descriptor flags; it does
            // not change them or depend on payload/thread scheduling.
            let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            OBSERVED
                .get()
                .unwrap()
                .send(flags & libc::O_NONBLOCK == 0)
                .unwrap();
            Ok(())
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let (sent, received) = std::sync::mpsc::channel();
        OBSERVED.set(sent).unwrap();
        let worker = std::thread::spawn(move || accept_loop(listener, worker_shutdown, observe));
        let client = TcpStream::connect(address).unwrap();
        let observed = received.recv_timeout(Duration::from_secs(3));
        shutdown.store(true, Ordering::Release);
        worker.join().unwrap();
        drop(client);
        assert!(
            observed.unwrap(),
            "blocking handlers must not inherit accept polling mode"
        );
    }
}
