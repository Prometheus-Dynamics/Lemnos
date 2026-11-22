//! A Unix-socket proxy for the bridge's tests: it stands between the bridge and a service it reaches by socket
//! (lemnosd, or the Orion node), copying bytes both ways. A test can hold the service's replies on the
//! connections whose client named itself with given bytes (a slow daemon call, as the bridge sees it), or cut
//! every connection there is (a broken session; the next connect is served again).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Rules {
    /// The next reply on a connection whose client sent these bytes is held this long, once.
    hold: Option<(Vec<u8>, Duration)>,
    /// When a held reply was let go, and how long it was held.
    held: Option<Duration>,
    /// Every live connection's two ends, for `cut`.
    open: Vec<(UnixStream, UnixStream)>,
}

pub struct Proxy {
    path: PathBuf,
    rules: Arc<Mutex<Rules>>,
    stop: Arc<AtomicBool>,
}

impl Proxy {
    /// Listens at `path` and serves each connection by connecting to `upstream`.
    pub fn start(path: &Path, upstream: &Path) -> Self {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).expect("the proxy's socket");
        listener.set_nonblocking(true).expect("nonblocking");
        let rules = Arc::new(Mutex::new(Rules::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (upstream, accept_rules, accept_stop) = (
            upstream.to_path_buf(),
            Arc::clone(&rules),
            Arc::clone(&stop),
        );
        std::thread::spawn(move || {
            while !accept_stop.load(Ordering::Relaxed) {
                let client = match listener.accept() {
                    Ok((client, _)) => client,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                };
                client.set_nonblocking(false).expect("blocking");
                let Ok(service) = UnixStream::connect(&upstream) else {
                    continue;
                };
                serve(client, service, &accept_rules);
            }
        });
        Self {
            path: path.to_path_buf(),
            rules,
            stop,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Holds the next reply, for `long`, on a connection whose client has sent `named` (its hello carries its
    /// name), once.
    pub fn hold_next(&self, named: &[u8], long: Duration) {
        let mut rules = self.rules.lock().unwrap();
        rules.hold = Some((named.to_vec(), long));
        rules.held = None;
    }

    /// How long the held reply was held, once it was let go.
    pub fn held(&self) -> Option<Duration> {
        self.rules.lock().unwrap().held
    }

    /// Cuts every connection there is now, both ends.
    pub fn cut(&self) {
        let open = std::mem::take(&mut self.rules.lock().unwrap().open);
        for (a, b) in open {
            let _ = a.shutdown(std::net::Shutdown::Both);
            let _ = b.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.cut();
    }
}

fn serve(client: UnixStream, service: UnixStream, rules: &Arc<Mutex<Rules>>) {
    rules.lock().unwrap().open.push((
        client.try_clone().expect("clone"),
        service.try_clone().expect("clone"),
    ));
    let sent = Arc::new(Mutex::new(Vec::<u8>::new()));
    {
        let (mut from, mut to, sent) = (
            client.try_clone().expect("clone"),
            service.try_clone().expect("clone"),
            Arc::clone(&sent),
        );
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = from.read(&mut buf) {
                if n == 0 {
                    break;
                }
                {
                    let mut sent = sent.lock().unwrap();
                    if sent.len() < 4096 {
                        sent.extend_from_slice(&buf[..n.min(4096)]);
                    }
                }
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Write);
        });
    }
    let (mut from, mut to, rules) = (service, client, Arc::clone(rules));
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            let hold = {
                let mut rules = rules.lock().unwrap();
                let mine = rules.hold.as_ref().is_some_and(|(named, _)| {
                    let sent = sent.lock().unwrap();
                    sent.windows(named.len()).any(|w| w == named.as_slice())
                });
                if mine {
                    rules.hold.take().map(|(_, long)| long)
                } else {
                    None
                }
            };
            if let Some(long) = hold {
                let since = Instant::now();
                std::thread::sleep(long);
                rules.lock().unwrap().held = Some(since.elapsed());
            }
            if to.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = to.shutdown(std::net::Shutdown::Write);
    });
}
