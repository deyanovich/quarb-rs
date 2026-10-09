//! The HTTP client the adapters share. Every request goes through
//! one agent per thread with a connection timeout and a read/write
//! timeout, so a service that stops answering fails the query with
//! a message instead of hanging it. `QUARB_HTTP_TIMEOUT` (seconds)
//! sets the read/write timeout; the connection timeout is 30 s.

pub use ureq;

/// The read/write timeout, in seconds, unless `QUARB_HTTP_TIMEOUT`
/// says otherwise.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

thread_local! {
    static AGENT: ureq::Agent = build();
}

fn build() -> ureq::Agent {
    let secs = std::env::var("QUARB_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(secs))
        .timeout_write(std::time::Duration::from_secs(secs))
        .build()
}

/// The shared agent: `quarb_http::agent().get(url).call()`.
pub fn agent() -> ureq::Agent {
    AGENT.with(|a| a.clone())
}
