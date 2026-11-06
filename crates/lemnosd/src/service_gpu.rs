//! The service's GPU lease glue (see `gpu.rs`).

use super::Service;
use crate::gpu::Out;
use lemnos_ipc::{Message, Request};

impl Service {
    /// A GPU lease request from client `ci`.
    pub(super) fn handle_gpu(&mut self, ci: usize, request: Request, now_ms: u64) {
        let (client, name) = (self.clients[ci].id, self.clients[ci].name.clone());
        let mut out = Out::new();
        match request {
            Request::GpuAcquire { id, acquire } => {
                self.gpu
                    .acquire((client, &name), id, acquire, now_ms, &mut out);
            }
            Request::GpuRelease { id, lease } => {
                let result = self.gpu.release(client, lease, now_ms, &mut out);
                out.insert(
                    0,
                    (
                        client,
                        Message::Reply {
                            id,
                            result: result.map(|()| 0.0),
                        },
                    ),
                );
            }
            Request::GpuHang { id, lease, detail } => {
                let result = self.gpu.hang((client, &name), lease, &detail, now_ms);
                out.push((
                    client,
                    Message::Reply {
                        id,
                        result: result.map(|()| 0.0),
                    },
                ));
            }
            Request::GpuStatus { id } => {
                let gpus = self.gpu.state(now_ms);
                out.push((client, Message::GpuState { id, gpus }));
            }
            _ => {}
        }
        self.send_gpu(out);
    }

    /// Overdue holders, and the grants a closed connection's leases free.
    pub(super) fn gpu_tick(&mut self, now_ms: u64, gone: &[u32]) {
        let mut out = Out::new();
        for &client in gone {
            self.gpu.client_gone(client, now_ms, &mut out);
        }
        self.gpu.tick(now_ms, &mut out);
        self.send_gpu(out);
    }

    fn send_gpu(&mut self, out: Out) {
        for (client, message) in out {
            if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
                c.send(&message);
            }
        }
    }
}
