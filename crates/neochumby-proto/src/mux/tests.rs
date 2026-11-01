use super::*;
use crate::{Identity, Msg, Session};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const WAIT: Duration = Duration::from_secs(10);

fn chumby_on(s: UnixStream) -> Line {
    Line::chumby(s.try_clone().unwrap(), s)
}

fn server_on(s: UnixStream) -> Line {
    Line::server(s.try_clone().unwrap(), s, |_| {})
}

/// A Chumby's line and neochumbyd's, joined.
fn pair() -> (Line, Line) {
    let (a, b) = UnixStream::pair().unwrap();
    (chumby_on(a), server_on(b))
}

/// Bytes no two runs share a pattern of.
fn bytes(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Echoes every channel neochumbyd's side takes, until the line goes.
fn echo(server: Line) {
    std::thread::spawn(move || {
        while let Some(c) = server.accept() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 3000];
                loop {
                    match (&c).read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if (&c).write_all(&buf[..n]).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
}

/// Writes `data` on `c` from one thread while reading it back on this one.
fn round_trip(c: Channel, data: Vec<u8>) -> Vec<u8> {
    let c = Arc::new(c);
    let w = c.clone();
    let len = data.len();
    let t = std::thread::spawn(move || (&*w).write_all(&data).unwrap());
    let mut got = vec![0u8; len];
    (&*c).read_exact(&mut got).unwrap();
    t.join().unwrap();
    got
}

#[test]
fn channels_carry_bytes_both_ways_however_many() {
    let (chumby, server) = pair();
    echo(server);
    let ts: Vec<_> = (0..4)
        .map(|i| {
            let chumby = chumby.clone();
            std::thread::spawn(move || {
                let c = chumby.open(WAIT).unwrap();
                let data = bytes(300_000 + i * 1000, i as u32 + 1);
                assert!(round_trip(c, data.clone()) == data, "channel {i} came back changed");
            })
        })
        .collect();
    for t in ts {
        t.join().unwrap();
    }
}

#[test]
fn a_reader_that_stops_holds_up_only_its_own_channel() {
    let (chumby, server) = pair();
    let stalled = Arc::new(AtomicBool::new(true));
    let s = stalled.clone();
    std::thread::spawn(move || {
        let first = server.accept().unwrap();
        // The first is never read until let go; the second echoes.
        let second = server.accept().unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; 100];
            let n = (&second).read(&mut b).unwrap();
            (&second).write_all(&b[..n]).unwrap();
            std::thread::sleep(WAIT);
        });
        while s.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut sink = Vec::new();
        (&first).read_to_end(&mut sink).unwrap();
        assert_eq!(sink.len(), WINDOW as usize + 1);
    });
    let first = chumby.open(WAIT).unwrap();
    let second = chumby.open(WAIT).unwrap();
    first.set_write_timeout(Some(Duration::from_millis(300))).unwrap();
    (&first).write_all(&vec![1; WINDOW as usize]).unwrap();
    let e = (&first).write(&[1]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::WouldBlock, "{e}");
    (&second).write_all(b"still here").unwrap();
    let mut b = [0u8; 10];
    (&second).read_exact(&mut b).unwrap();
    assert_eq!(&b, b"still here");
    stalled.store(false, Ordering::SeqCst);
    first.set_write_timeout(None).unwrap();
    (&first).write_all(&[1]).unwrap();
    drop(first);
}

#[test]
fn what_was_written_arrives_before_the_end() {
    let (chumby, server) = pair();
    let data = bytes(100_000, 7);
    let want = data.clone();
    let t = std::thread::spawn(move || {
        let c = server.accept().unwrap();
        let mut got = Vec::new();
        (&c).read_to_end(&mut got).unwrap();
        got
    });
    let c = chumby.open(WAIT).unwrap();
    (&c).write_all(&data).unwrap();
    drop(c);
    assert!(t.join().unwrap() == want);
}

#[test]
fn reads_time_out_and_lose_nothing() {
    let (chumby, server) = pair();
    let c = chumby.open(WAIT).unwrap();
    let s = server.accept().unwrap();
    c.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    assert_eq!((&c).read(&mut [0u8; 4]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    (&s).write_all(b"late").unwrap();
    let mut b = [0u8; 4];
    (&c).read_exact(&mut b).unwrap();
    assert_eq!(&b, b"late");
}

#[test]
fn with_no_neochumbyd_an_open_is_refused() {
    let (a, _b) = UnixStream::pair().unwrap();
    let chumby = chumby_on(a);
    let e = chumby.open(Duration::from_millis(200)).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused, "{e}");
}

#[test]
fn neochumbyd_takes_no_more_channels_than_it_may() {
    let (chumby, server) = pair();
    let open: Vec<Channel> = (0..MAX_CHANNELS).map(|_| chumby.open(WAIT).unwrap()).collect();
    let e = chumby.open(WAIT).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::Other, "{e}");
    drop(open);
    drop(server);
}

/// Between a Chumby and whichever neochumbyd is plugged in now, as an
/// emulator's chardev is: what the Chumby writes with none plugged in is
/// lost. `corrupt` flips the byte at that offset of what the Chumby sends.
struct Wire {
    chumby: UnixStream,
    server: Arc<Mutex<Option<UnixStream>>>,
}

impl Wire {
    fn new(corrupt: Option<usize>) -> (Wire, Line) {
        let (a, b) = UnixStream::pair().unwrap();
        let server: Arc<Mutex<Option<UnixStream>>> = Arc::default();
        let (mut from_chumby, to) = (b.try_clone().unwrap(), server.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let mut at = 0;
            while let Ok(n) = from_chumby.read(&mut buf) {
                if n == 0 {
                    return;
                }
                if let Some(c) = corrupt
                    && (at..at + n).contains(&c)
                {
                    buf[c - at] ^= 0x40;
                }
                at += n;
                if let Some(s) = to.lock().unwrap().as_mut() {
                    let _ = s.write_all(&buf[..n]);
                }
            }
        });
        (Wire { chumby: b, server }, chumby_on(a))
    }

    /// Plugs in a new neochumbyd, unplugging the last.
    fn plug(&self) -> Line {
        let (a, b) = UnixStream::pair().unwrap();
        let mut to_chumby = self.chumby.try_clone().unwrap();
        let mut from_server = a.try_clone().unwrap();
        if let Some(old) = self.server.lock().unwrap().replace(a) {
            let _ = old.shutdown(std::net::Shutdown::Both);
        }
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = from_server.read(&mut buf) {
                if n == 0 || to_chumby.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
        });
        server_on(b)
    }
}

#[test]
fn a_new_neochumbyd_ends_the_channels_and_takes_new_ones() {
    let (wire, chumby) = Wire::new(None);
    echo(wire.plug());
    let c = chumby.open(WAIT).unwrap();
    assert!(round_trip(c, b"first".to_vec()) == b"first");
    let held = chumby.open(WAIT).unwrap();
    echo(wire.plug());
    let e = (&held).read(&mut [0u8; 1]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset, "{e}");
    let c = chumby.open(WAIT).unwrap();
    assert!(round_trip(c, b"second".to_vec()) == b"second");
}

#[test]
fn a_byte_changed_on_the_line_brings_it_back_in_step() {
    let (wire, chumby) = Wire::new(Some(20_000));
    echo(wire.plug());
    let resets = Arc::new(AtomicUsize::new(0));
    let mut done = false;
    for i in 0..20 {
        let Ok(c) = chumby.open(WAIT) else { continue };
        let c = Arc::new(c);
        let w = c.clone();
        let data = bytes(8000, i + 1);
        let want = data.clone();
        let t = std::thread::spawn(move || {
            let _ = (&*w).write_all(&data);
        });
        let mut got = vec![0u8; want.len()];
        c.set_read_timeout(Some(WAIT)).unwrap();
        match (&*c).read_exact(&mut got) {
            Ok(()) => {
                assert!(got == want, "a channel carried changed bytes");
                if resets.load(Ordering::SeqCst) > 0 {
                    done = true;
                    let _ = t.join();
                    break;
                }
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::ConnectionReset, "{e}");
                resets.fetch_add(1, Ordering::SeqCst);
            }
        }
        let _ = t.join();
    }
    assert!(done, "the line never lost step, or never came back ({} resets)", resets.load(Ordering::SeqCst));
}

#[test]
fn a_session_runs_over_a_channel_as_over_tcp() {
    let (chumby, server) = pair();
    let (sid, cid) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let (sp, cp) = (sid.public(), cid.public());
    let t = std::thread::spawn(move || {
        let c = server.accept().unwrap();
        let by = std::time::Instant::now() + Duration::from_secs(20);
        let mut s = Session::accept(c, &sid, by, |k| k == &cp).unwrap();
        assert_eq!(s.recv().unwrap(), Msg::Hello { name: "den".into() });
        s.send(&Msg::Data(bytes(60_000, 3))).unwrap();
        s.recv().unwrap()
    });
    let c = chumby.open(WAIT).unwrap();
    let mut s = Session::connect(c, &cid, &sp).unwrap();
    s.send(&Msg::Hello { name: "den".into() }).unwrap();
    assert!(s.recv().unwrap() == Msg::Data(bytes(60_000, 3)));
    s.send(&Msg::Closed).unwrap();
    assert_eq!(t.join().unwrap(), Msg::Closed);
}

#[test]
fn frames_check_out() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    let mut d = Decoder::default();
    // Out of step, everything before a SYNC is dropped.
    d.push(b"noise");
    d.push(&frame(DATA, 1, b"lost"));
    let sync = sync_frame(5, None, false);
    d.push(&sync[..10]);
    assert!(d.next().ok().unwrap().is_none());
    d.push(&sync[10..]);
    d.push(&frame(DATA, 1, b"kept"));
    assert_eq!(d.next().ok().unwrap().unwrap().0, SYNC);
    assert_eq!(d.next().ok().unwrap().unwrap(), (DATA, 1, b"kept".to_vec()));
    let mut bad = frame(DATA, 1, b"bent");
    bad[6] ^= 1;
    d.push(&bad);
    assert!(d.next().is_err());
}
