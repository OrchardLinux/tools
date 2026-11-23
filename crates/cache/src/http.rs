//! Reading byte ranges of a remote file over HTTPS.
use crate::source::Source;
use crate::{Code, Error, Result};
use std::io::Read;
use std::time::Duration;

/// Tries per range. Only connection failures, 429 and 5xx are retried.
const ATTEMPTS: u32 = 3;
/// The slowest body rate a range may have, in bytes per second. A slow link
/// that keeps delivering succeeds; a stalled one fails.
const MIN_RATE: u64 = 64 * 1024;

/// A remote file. The caller knows its size from the catalog, so the
/// server's idea of the size is checked, not trusted.
pub struct Http {
    agent: ureq::Agent,
    url: String,
    size: u64,
    timeout: Duration,
    /// ETag or Last-Modified of the first response. A different one later
    /// means the file changed, and members would mix two versions.
    version: Option<String>,
}

impl Http {
    /// `timeout` bounds connecting, the response headers and each body read.
    pub fn new(url: &str, size: u64, timeout: Duration) -> Result<Http> {
        Ok(Http {
            agent: agent(url, timeout)?,
            url: url.into(),
            size,
            timeout,
            version: None,
        })
    }

    /// Opens a remote file whose size is not known in advance, asking the
    /// server for it.
    pub fn probe(url: &str, timeout: Duration) -> Result<Http> {
        let agent = agent(url, timeout)?;
        let response = agent
            .get(url)
            .header("Range", "bytes=0-0")
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| match e {
                ureq::Error::StatusCode(404) => Error::new(Code::NotFound, format!("{url} is not there (HTTP 404)")),
                e => Error::new(Code::Network, e.to_string()),
            })?;
        let size = response
            .headers()
            .get("Content-Range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes 0-0/"))
            .and_then(|n| n.parse().ok())
            .filter(|_| response.status().as_u16() == 206)
            .ok_or_else(|| Error::new(Code::Network, "server does not support byte ranges"))?;
        Ok(Http {
            agent,
            url: url.into(),
            size,
            timeout,
            version: None,
        })
    }

    fn request(&mut self, offset: u64, len: usize) -> std::result::Result<Vec<u8>, Failure> {
        let last = offset + len as u64 - 1;
        let deadline = self.timeout + Duration::from_secs((len as u64).div_ceil(MIN_RATE));
        let mut response = self
            .agent
            .get(&self.url)
            .header("Range", format!("bytes={offset}-{last}"))
            .header("Accept-Encoding", "identity")
            .config()
            .timeout_per_call(Some(deadline))
            .build()
            .call()
            .map_err(Failure::from)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let reply = Reply {
            status: response.status().as_u16(),
            content_range: header("Content-Range"),
            content_encoding: header("Content-Encoding"),
            version: header("ETag").or_else(|| header("Last-Modified")),
        };
        reply
            .check(offset, last, self.size, &mut self.version)
            .map_err(Failure::Final)?;
        let mut data = Vec::with_capacity(len);
        response
            .body_mut()
            .as_reader()
            .take(len as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| Failure::Retry(e.to_string()))?;
        if data.len() != len {
            return Err(Failure::Final(Error::new(
                Code::Network,
                "range response has the wrong length",
            )));
        }
        Ok(data)
    }
}

pub fn agent(url: &str, timeout: Duration) -> Result<ureq::Agent> {
    if !url.starts_with("https://") {
        return Err(Error::new(
            Code::InvalidInput,
            "firmware URLs must use HTTPS",
        ));
    }
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_resolve(Some(timeout))
        .timeout_connect(Some(timeout))
        .timeout_send_request(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_per_read(Some(timeout))
        .build();
    Ok(config.into())
}

impl Source for Http {
    fn size(&self) -> u64 {
        self.size
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        for attempt in 1.. {
            match self.request(offset, len) {
                Ok(data) => return Ok(data),
                Err(Failure::Retry(_)) if attempt < ATTEMPTS => {
                    std::thread::sleep(Duration::from_millis(500 << (attempt - 1)));
                }
                Err(Failure::Retry(message)) => return Err(Error::new(Code::Network, message)),
                Err(Failure::Final(e)) => return Err(e),
            }
        }
        unreachable!()
    }
}

enum Failure {
    Retry(String),
    Final(Error),
}

impl From<ureq::Error> for Failure {
    fn from(e: ureq::Error) -> Failure {
        use ureq::Error::*;
        match e {
            StatusCode(code) if code == 429 || code >= 500 => Failure::Retry(e.to_string()),
            Io(_) | Timeout(_) | ConnectionFailed | HostNotFound | BodyStalled => {
                Failure::Retry(e.to_string())
            }
            _ => Failure::Final(Error::new(Code::Network, e.to_string())),
        }
    }
}

/// The parts of a range response that decide whether its body can be used.
struct Reply {
    status: u16,
    content_range: Option<String>,
    content_encoding: Option<String>,
    version: Option<String>,
}

impl Reply {
    fn check(&self, offset: u64, last: u64, size: u64, seen: &mut Option<String>) -> Result<()> {
        let expected = format!("bytes {offset}-{last}/{size}");
        if self.status != 206 || self.content_range.as_deref() != Some(&expected) {
            return Err(Error::new(
                Code::Network,
                "server did not return the requested byte range",
            ));
        }
        if self
            .content_encoding
            .as_deref()
            .is_some_and(|e| e != "identity")
        {
            return Err(Error::new(
                Code::Network,
                "server compressed the byte range",
            ));
        }
        match (&*seen, &self.version) {
            (Some(first), Some(now)) if first != now => Err(Error::new(
                Code::Network,
                "remote file changed while reading it",
            )),
            (None, Some(now)) => {
                *seen = Some(now.clone());
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, range: &str, version: &str) -> Reply {
        Reply {
            status,
            content_range: Some(range.into()),
            content_encoding: None,
            version: Some(version.into()),
        }
    }

    #[test]
    fn accepts_the_exact_range() {
        let mut seen = None;
        reply(206, "bytes 10-19/100", "\"a\"")
            .check(10, 19, 100, &mut seen)
            .unwrap();
        assert_eq!(seen.as_deref(), Some("\"a\""));
    }

    #[test]
    fn rejects_whole_file_other_range_or_size() {
        let mut seen = None;
        assert!(reply(200, "bytes 10-19/100", "a")
            .check(10, 19, 100, &mut seen)
            .is_err());
        assert!(reply(206, "bytes 10-20/100", "a")
            .check(10, 19, 100, &mut seen)
            .is_err());
        assert!(reply(206, "bytes 10-19/101", "a")
            .check(10, 19, 100, &mut seen)
            .is_err());
    }

    #[test]
    fn rejects_compression_and_a_changed_file() {
        let mut seen = Some("a".to_owned());
        let mut compressed = reply(206, "bytes 0-9/100", "a");
        compressed.content_encoding = Some("gzip".into());
        assert!(compressed.check(0, 9, 100, &mut seen).is_err());
        assert!(reply(206, "bytes 0-9/100", "b")
            .check(0, 9, 100, &mut seen)
            .is_err());
    }

    #[test]
    fn plain_http_is_refused() {
        assert!(Http::new("http://example.com/x.ipsw", 10, Duration::from_secs(1)).is_err());
    }
}
