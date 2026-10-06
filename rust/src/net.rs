//! 网络请求公共设施：IPv4 优先解析。
//! 运营商的 IPv6 出口对部分境外 CDN（如 Cloudflare）路由劣化时连接会长时间挂起，
//! 而系统解析把 AAAA 排在前面，默认 Agent 会先卡在 IPv6 上耗尽总超时——
//! 全应用 HTTP 请求统一走这里的 IPv4 优先 Agent。

/// 解析域名：IPv4 地址排前、IPv6 排后（各自保持原顺序）
pub fn resolve_v4_first(netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    let mut addrs: Vec<_> = std::net::ToSocketAddrs::to_socket_addrs(netloc)?.collect();
    addrs.sort_by_key(|a| !a.is_ipv4());
    Ok(addrs)
}

/// 全应用共用 Agent（IPv4 优先解析）
pub fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| ureq::AgentBuilder::new().resolver(resolve_v4_first).build())
}
