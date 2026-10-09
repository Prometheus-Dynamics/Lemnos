//! The service's raw-access and override glue: raw requests, edges,
//! restoring control writes, and what a closed connection leaves behind.

use super::Service;
use crate::clients::{Client, Requester};
use crate::raw::Context;
use lemnos_ipc::{Event, Message, RawRequest, Refusal};

impl Service {
    pub(super) fn handle_raw(&mut self, ci: usize, request: RawRequest) {
        let (id, name, keep) = {
            let c = &self.clients[ci];
            (c.id, c.name.clone(), c.keep)
        };
        let who = Requester {
            id,
            name: &name,
            keep,
        };
        let mut ctx = Context {
            buses: &mut *self.buses,
            board: &self.definition,
            registry: &self.registry,
        };
        let message = self.raw.handle(request, &who, &mut ctx);
        self.clients[ci].send(&message);
    }

    /// Sends pending edges to the clients that claimed the lines.
    pub(super) fn send_edges(&mut self) {
        for (client, event) in self.raw.take_edges() {
            if let Some(c) = self
                .clients
                .iter_mut()
                .find(|c| c.id == client && c.greeted)
            {
                c.send(&Message::Event(event));
            }
        }
    }

    /// `Request::Restore`: undoes the requester's writes to a device (by
    /// name, so a one-shot client such as `lemnos-ctl` can undo an earlier
    /// run's writes).
    pub(super) fn restore_control(
        &mut self,
        ci: usize,
        device: &str,
        control: &str,
    ) -> Result<(), Refusal> {
        let index = self.slot_of(device).ok_or(Refusal::UnknownDevice)?;
        let control_index = match control {
            "" => None,
            name => Some(
                self.slots[index]
                    .info
                    .and_then(|i| i.control_index(name))
                    .ok_or(Refusal::UnknownControl)?,
            ),
        };
        let name = self.clients[ci].name.clone();
        let reverted = self.slots[index]
            .revert(|o| o.name == name && control_index.is_none_or(|c| c == o.control));
        if reverted {
            if self.slots[index].restore.is_some() {
                self.save_fan_state();
            }
            self.broadcast(&Message::Event(Event::Control {
                device: device.to_string(),
                control: if control.is_empty() {
                    "restore".into()
                } else {
                    control.to_string()
                },
                value: 0.0,
                by: name,
            }));
        }
        Ok(())
    }

    /// A connection closed: its claims end (or wait, for clients that keep
    /// their intents), and its control writes are undone.
    pub(super) fn client_closed(&mut self, client: &Client) {
        self.raw.client_gone(client.id, client.keep);
        if client.keep {
            return;
        }
        let mut fan = false;
        for index in 0..self.slots.len() {
            if self.slots[index].revert(|o| o.holder == client.id && !o.keep) {
                fan |= self.slots[index].restore.is_some();
                let device = self.slots[index].id().to_string();
                self.broadcast(&Message::Event(Event::Control {
                    device,
                    control: "restore".into(),
                    value: 0.0,
                    by: client.name.clone(),
                }));
            }
        }
        if fan {
            self.save_fan_state();
        }
    }
}
