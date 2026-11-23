//! The host side against a fake receiver that speaks the contract from the
//! device's side, over the same framing.
use super::*;
use serde_json::json;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

struct FakeOut(Sender<Vec<u8>>);
impl Outbound for FakeOut {
    fn write(&mut self, data: &[u8], _: Duration) -> Result<(), Fault> {
        self.0.send(data.to_vec()).map_err(|_| Fault::new(Kind::Disconnected, "fake gone"))
    }
}

struct FakeIn(Receiver<Vec<u8>>);
impl Inbound for FakeIn {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, Fault> {
        match self.0.recv_timeout(timeout) {
            Ok(b) => Ok(b),
            Err(RecvTimeoutError::Timeout) => Err(Fault::new(Kind::Timeout, "quiet")),
            Err(RecvTimeoutError::Disconnected) => Err(Fault::new(Kind::Disconnected, "unplugged")),
        }
    }
}

/// How the fake behaves beyond the contract's happy path.
#[derive(Clone, Copy, Default)]
struct Script {
    protocol: u64,
    refuse_plan: bool,
    silent: bool,
    sha_mismatch: bool,
}

#[derive(Default)]
struct Seen {
    staged: Vec<(String, Vec<u8>)>,
    streamed: Vec<u8>,
    plan: Option<Value>,
}

fn reply(to: &Sender<Vec<u8>>, kind: u8, body: &Value) {
    let p = body.to_string().into_bytes();
    let mut out = vec![kind, 0, 0, 0];
    out.extend_from_slice(&(p.len() as u32).to_le_bytes());
    out.extend_from_slice(&p);
    // Split oddly, as USB transfers may.
    let (a, b) = out.split_at(out.len() / 3);
    let _ = to.send(a.to_vec());
    let _ = to.send(b.to_vec());
}

fn fake(script: Script) -> (Session, Arc<Mutex<Seen>>) {
    let (host_tx, dev_rx) = mpsc::channel::<Vec<u8>>();
    let (dev_tx, host_rx) = mpsc::channel::<Vec<u8>>();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = seen.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        // (is_stream, name, bytes left, collected)
        let mut transfer: Option<(bool, String, u64, Vec<u8>, String)> = None;
        while let Ok(bytes) = dev_rx.recv() {
            buf.extend(bytes);
            while buf.len() >= 8 {
                let len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
                if buf.len() < 8 + len {
                    break;
                }
                let kind = buf[0];
                let payload: Vec<u8> = buf[8..8 + len].to_vec();
                buf.drain(..8 + len);
                if script.silent {
                    continue;
                }
                reply(&dev_tx, 0x83, &json!({"event": "heartbeat", "phase": "ready"}));
                let body: Value = if kind == 0x06 { Value::Null } else { serde_json::from_slice(&payload).unwrap() };
                match kind {
                    0x01 => reply(&dev_tx, 0x81, &json!({"protocol": script.protocol, "ecid": "1a2b", "board": "j71b",
                        "boot_id": "b", "phase": "ready", "staged": [], "stream": null, "record": null, "error": null})),
                    0x02 => reply(&dev_tx, 0x81, &json!({"disk": {"bytes": 128_000_000_000u64}, "partitions": [], "free": []})),
                    0x04 if script.refuse_plan => reply(&dev_tx, 0x82, &json!({"code": "layout-changed", "message": "the table changed"})),
                    0x04 => {
                        record.lock().unwrap().plan = Some(body);
                        reply(&dev_tx, 0x81, &json!({"root": "/dev/nvme0n1p5", "boot": "/dev/nvme0n1p4"}));
                    }
                    0x03 | 0x05 => {
                        let name = body.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                        let sha = body["sha256"].as_str().unwrap().to_owned();
                        transfer = Some((kind == 0x05, name, body["bytes"].as_u64().unwrap(), Vec::new(), sha));
                        reply(&dev_tx, 0x81, &json!({}));
                    }
                    0x06 => {
                        let (is_stream, name, left, mut got, sha) = transfer.take().unwrap();
                        got.extend_from_slice(&payload);
                        let left = left - payload.len() as u64;
                        if is_stream {
                            reply(&dev_tx, 0x83, &json!({"event": "stream", "written": got.len(), "bytes": got.len() as u64 + left}));
                        }
                        if left > 0 {
                            transfer = Some((is_stream, name, left, got, sha));
                        } else if script.sha_mismatch || sha == "bad" {
                            reply(&dev_tx, 0x82, &json!({"code": "sha-mismatch", "message": "no match"}));
                        } else {
                            let mut s = record.lock().unwrap();
                            if is_stream { s.streamed = got } else { s.staged.push((name, got)) }
                            reply(&dev_tx, 0x81, &json!({}));
                            if is_stream {
                                reply(&dev_tx, 0x83, &json!({"event": "step", "step": "account"}));
                                reply(&dev_tx, 0x83, &json!({"event": "done", "record": {"entry": "omarchy-a-x"}}));
                            }
                        }
                    }
                    _ => reply(&dev_tx, 0x82, &json!({"code": "unknown-request", "message": "?"})),
                }
            }
        }
    });
    (Session::new(FakeOut(host_tx), FakeIn(host_rx)), seen)
}

fn ok() -> Script {
    Script { protocol: 1, ..Script::default() }
}

#[test]
fn info_reads_the_state_and_checks_the_version() {
    let (mut s, _) = fake(ok());
    let info = s.info().unwrap();
    assert_eq!((info.board.as_str(), info.phase), ("j71b", Phase::Ready));
    let (mut s, _) = fake(Script { protocol: 2, ..ok() });
    assert!(matches!(s.info(), Err(Failure::Version(2))));
}

#[test]
fn inspect_and_plan_round_trip() {
    let (mut s, seen) = fake(ok());
    assert_eq!(s.inspect().unwrap()["disk"]["bytes"], 128_000_000_000u64);
    let nodes = s.plan(&json!({"version": 2, "root": {"blocks": 1}})).unwrap();
    assert_eq!(nodes["root"], "/dev/nvme0n1p5");
    assert_eq!(seen.lock().unwrap().plan.as_ref().unwrap()["version"], 2);
}

#[test]
fn a_refused_plan_reports_its_code() {
    let (mut s, _) = fake(Script { refuse_plan: true, ..ok() });
    match s.plan(&json!({})) {
        Err(Failure::Refused(r)) => assert_eq!(r.code, "layout-changed"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn put_sends_the_file_in_messages_of_at_most_one_mib() {
    let (mut s, seen) = fake(ok());
    let data: Vec<u8> = (0..(MAX_PAYLOAD * 2 + 123)).map(|i| i as u8).collect();
    s.put("layer.tar.gz", data.len() as u64, "abc", &mut data.as_slice()).unwrap();
    let staged = &seen.lock().unwrap().staged;
    assert_eq!(staged[0].0, "layer.tar.gz");
    assert_eq!(staged[0].1, data);
}

#[test]
fn stream_reports_progress_steps_and_the_record_as_events() {
    let (mut s, seen) = fake(ok());
    let events = Arc::new(Mutex::new(Vec::new()));
    let log = events.clone();
    s.on_event(move |e| log.lock().unwrap().push(e.clone()));
    let image = vec![7u8; MAX_PAYLOAD + 10];
    s.stream(image.len() as u64, "abc", &mut image.as_slice()).unwrap();
    assert_eq!(seen.lock().unwrap().streamed.len(), image.len());
    // The finishing steps and `done` come after the last answer, with no
    // request in flight (as in a real install): they must arrive by themselves.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !events.lock().unwrap().iter().any(|e| matches!(e, Event::Done(_))) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Stream { written, .. } if *written as usize == image.len())));
    assert!(events.contains(&Event::Step("account".into())));
    assert!(events.iter().any(|e| matches!(e, Event::Done(r) if r["entry"] == "omarchy-a-x")));
    assert!(events.iter().any(|e| matches!(e, Event::Heartbeat { phase: Phase::Ready })));
}

#[test]
fn a_bad_hash_is_a_refusal() {
    let (mut s, _) = fake(Script { sha_mismatch: true, ..ok() });
    match s.put("x", 3, "abc", &mut &b"abc"[..]) {
        Err(Failure::Refused(r)) => assert_eq!(r.code, "sha-mismatch"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_silent_device_counts_as_lost() {
    let (mut s, _) = fake(Script { silent: true, ..ok() });
    let started = std::time::Instant::now();
    let err = s.info().unwrap_err();
    assert!(err.lost(), "{err}");
    assert!(started.elapsed() >= SILENCE);
}

#[test]
fn events_parse_and_unknown_ones_pass_through() {
    assert_eq!(Event::parse(json!({"event": "step", "step": "sync"})), Event::Step("sync".into()));
    assert!(matches!(Event::parse(json!({"event": "failed", "code": "finish", "message": "m"})), Event::Failed(r) if r.code == "finish"));
    assert!(matches!(Event::parse(json!({"event": "new-thing"})), Event::Other(_)));
}
