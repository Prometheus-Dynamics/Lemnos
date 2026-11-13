//! `lemnos-ctl`: everything it knows about a device comes from that device's
//! description, so it works on devices nobody has written tooling for.

use std::io::Write;
use std::time::Duration;

use lemnos_client::{Client, DeviceEntry, DeviceHealth, DeviceState, Error, PackageMode, follow};
use lemnos_core::ItemKind;
use lemnos_wire::text::{
    encode_args, find_item, find_method, method_of, render, render_reply, shape_name,
};
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod, OwnedShape};

pub const USAGE: &str = "\
lemnos-ctl [--socket PATH] COMMAND
  list                           every device and how it stands
  chain DEVICE                   the devices it leads to through `link`, in order
  describe DEVICE                what it provides
  read DEVICE [ITEM]             one value, or all of them
  watch DEVICE ITEM [--period-ms N] [--count N]
  set DEVICE ITEM [VALUE...]     a control or an action
  call DEVICE METHOD [ARG...]    any method, as NAME or TAG:NAME
  off DEVICE | on DEVICE
  packages                       driver packages: which loaded, which were refused and why
  validate BOARD                 check a board file and show the host settings it gives, with no
                                 daemon";

const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
const REPLY_MAX: usize = 64 * 1024;
const WATCH_WAIT: Duration = Duration::from_secs(2);

type Outcome = Result<(), String>;

fn text(error: Error) -> String {
    error.to_string()
}

fn line(out: &mut dyn Write, message: core::fmt::Arguments<'_>) -> Outcome {
    writeln!(out, "{message}").map_err(|e| e.to_string())
}

const fn state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Running => "running",
        DeviceState::BackingOff => "retrying",
        DeviceState::Off => "off",
    }
}

const fn health_name(health: DeviceHealth) -> &'static str {
    match health {
        DeviceHealth::Ok => "ok",
        DeviceHealth::Degraded => "degraded",
        DeviceHealth::Failed => "failed",
    }
}

struct Tool<'a> {
    client: Client,
    out: &'a mut dyn Write,
}

/// Runs one command. `args` excludes the program's name.
pub fn run(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut socket = std::env::var("LEMNOSD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
    let mut rest = args;
    if let [flag, path, tail @ ..] = rest
        && flag == "--socket"
    {
        socket.clone_from(path);
        rest = tail;
    }
    let [command, tail @ ..] = rest else {
        return Err(USAGE.to_string());
    };
    if command == "validate" {
        return match tail {
            [board] => validate(board, out),
            _ => Err(USAGE.to_string()),
        };
    }
    let client = Client::connect(&socket, "lemnos-ctl").map_err(|e| format!("{socket}: {e}"))?;
    let mut tool = Tool { client, out };
    match (command.as_str(), tail) {
        ("list", []) => tool.list(),
        ("chain", [device]) => tool.chain(device),
        ("describe", [device]) => tool.describe(device),
        ("read", [device]) => tool.read_all(device),
        ("read", [device, item]) => tool.read_one(device, item),
        ("watch", [device, item, options @ ..]) => tool.watch(device, item, options),
        ("set", [device, item, values @ ..]) => tool.set(device, item, values),
        ("call", [device, method, values @ ..]) => tool.call(device, method, values),
        ("off", [device]) => tool.power(device, false),
        ("on", [device]) => tool.power(device, true),
        ("packages", []) => tool.packages(),
        _ => Err(USAGE.to_string()),
    }
}

/// Reads and checks the board file at `path` as `lemnosd` would, and prints what the host will
/// run with: its settings, defaults filled in, and the devices that opted in to auto-off.
fn validate(path: &str, out: &mut dyn Write) -> Outcome {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let options = board.host.apply(lemnos_host::Options::default());
    line(
        out,
        format_args!(
            "{path}: board {:?}, {} devices",
            board.board.id,
            board.devices.len()
        ),
    )?;
    line(
        out,
        format_args!(
            "host: call deadline {} ms, first retry {} ms, retry cap {} ms",
            options.call_deadline.as_millis(),
            options.retry_first.as_millis(),
            options.retry_cap.as_millis(),
        ),
    )?;
    let opted: Vec<String> = board
        .devices
        .iter()
        .filter_map(|d| d.auto_off_after.map(|n| format!("{} after {n}", d.id)))
        .collect();
    if opted.is_empty() {
        line(
            out,
            format_args!("auto-off: none, every device is retried forever"),
        )
    } else {
        line(out, format_args!("auto-off: {}", opted.join(", ")))
    }
}

impl Tool<'_> {
    fn device(&self, name: &str) -> Result<DeviceEntry, String> {
        self.client
            .list()
            .map_err(text)?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or_else(|| format!("no device {name:?}"))
    }

    fn description(&self, name: &str) -> Result<(DeviceEntry, DeviceDescription), String> {
        let device = self.device(name)?;
        let description = self.client.describe(device.index).map_err(text)?;
        Ok((device, description))
    }

    fn list(&mut self) -> Outcome {
        let devices = self.client.list().map_err(text)?;
        let width = devices.iter().map(|d| d.name.len()).max().unwrap_or(0);
        for d in devices {
            let tags = d.tags.join(",");
            let mut row = format!(
                "{:width$}  {}  {}  {}  {tags}",
                d.name,
                d.driver,
                state_name(d.state),
                health_name(d.health),
            );
            if let Some(link) = &d.link {
                row.push_str(&format!("  -> {link}"));
            }
            if !d.reason.is_empty() {
                row.push_str(&format!("  ({})", d.reason));
            }
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    /// What `name` is wired to, and what that is wired to, one device a row: a camera sensor,
    /// its receiver, the receiver's ISP. The tool knows none of them: it follows the board's
    /// `link` from one device to the next.
    fn chain(&mut self, name: &str) -> Outcome {
        let devices = self.client.list().map_err(text)?;
        let chain = follow(&devices, name);
        let Some(last) = chain.last() else {
            return Err(format!("no device {name:?}"));
        };
        let width = chain.iter().map(|d| d.name.len()).max().unwrap_or(0);
        for d in &chain {
            let tags = d.tags.join(",");
            line(
                self.out,
                format_args!(
                    "{:width$}  {}  {}  {}  {tags}",
                    d.name,
                    d.driver,
                    state_name(d.state),
                    health_name(d.health),
                ),
            )?;
        }
        // Where it ends says why: nothing further, a name the board doesn't have, or a circle.
        match &last.link {
            None => Ok(()),
            Some(link) if devices.iter().any(|d| &d.name == link) => {
                line(self.out, format_args!("-> {link}: already above"))
            }
            Some(link) => line(self.out, format_args!("-> {link}: no such device")),
        }
    }

    /// How the service runs driver packages, then every file it looked at: the driver a loaded
    /// one became, or why a refused one was refused.
    fn packages(&mut self) -> Outcome {
        let list = self.client.packages().map_err(text)?;
        let mode = match list.mode {
            PackageMode::Off => "off: no package directory is configured",
            PackageMode::Signed => "signed: only packages signed by a trusted key run",
            PackageMode::Open => "open: any package in the directory runs",
        };
        line(self.out, format_args!("packages {mode}"))?;
        if !list.note.is_empty() {
            line(self.out, format_args!("{}", list.note))?;
        }
        let width = list
            .packages
            .iter()
            .map(|p| p.path.len())
            .max()
            .unwrap_or(0);
        for package in list.packages {
            let row = match package.driver {
                Some(driver) => format!("{:width$}  loaded   {driver}", package.path),
                None => format!("{:width$}  refused  {}", package.path, package.reason),
            };
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    fn describe(&mut self, name: &str) -> Outcome {
        let (device, description) = self.description(name)?;
        line(
            self.out,
            format_args!("{} ({})", device.name, device.driver),
        )?;
        for contract in &description.contracts {
            line(self.out, format_args!("{}", contract.tag))?;
            for method in &contract.methods {
                let params = method
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, shape_name(&p.shape)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let returns = shape_name(&method.returns);
                line(
                    self.out,
                    format_args!("  {}({params}) -> {returns}", method.name),
                )?;
            }
        }
        for item in &description.items {
            let kind = match item.kind {
                ItemKind::Value => "value",
                ItemKind::Control => "control",
                ItemKind::Action => "action",
            };
            let mut row = format!("{kind} {}", item.name);
            if !item.unit.is_empty() {
                row.push_str(&format!(" [{}]", item.unit));
            }
            if let (Some(min), Some(max)) = (item.min, item.max) {
                row.push_str(&format!(" {min}..{max}"));
            }
            line(self.out, format_args!("{row}"))?;
        }
        Ok(())
    }

    fn invoke(
        &self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
        args: &[u8],
    ) -> Result<Vec<u8>, String> {
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let mut reply = vec![0; REPLY_MAX];
        let n = self
            .client
            .call(&mut handle, method.id, args, &mut reply)
            .map_err(text)?;
        reply.truncate(n);
        Ok(reply)
    }

    fn read_item(
        &mut self,
        device: &str,
        description: &DeviceDescription,
        item: &OwnedItem,
    ) -> Outcome {
        let (contract, method) = method_of(description, item)?;
        if !method.params.is_empty() {
            return Err(format!("{} takes arguments; use call", item.name));
        }
        let reply = self.invoke(device, description, contract, method, &[])?;
        self.print_value(item, method, &reply)
    }

    fn print_value(&mut self, item: &OwnedItem, method: &OwnedMethod, reply: &[u8]) -> Outcome {
        let value = render(&method.returns, method.has_out_buffer, reply, item)?;
        line(self.out, format_args!("{} = {value}", item.name))
    }

    fn read_all(&mut self, name: &str) -> Outcome {
        let (_, description) = self.description(name)?;
        for item in description
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Value)
        {
            self.read_item(name, &description, item)?;
        }
        Ok(())
    }

    fn read_one(&mut self, name: &str, wanted: &str) -> Outcome {
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        self.read_item(name, &description, item)
    }

    fn watch(&mut self, name: &str, wanted: &str, options: &[String]) -> Outcome {
        let mut period_ms = 100u64;
        let mut count = u64::MAX;
        let mut options = options.iter();
        while let Some(option) = options.next() {
            let value = options.next().ok_or_else(|| USAGE.to_string())?;
            let number = value.parse().map_err(|_| format!("{value:?}: a number"))?;
            match option.as_str() {
                "--period-ms" => period_ms = number,
                "--count" => count = number,
                _ => return Err(USAGE.to_string()),
            }
        }
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        let (contract, method) = method_of(&description, item)?;
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(name, tag).map_err(text)?;
        self.client
            .subscribe(&mut handle, method.id, period_ms * 1000, false)
            .map_err(text)?;
        let mut into = vec![0; REPLY_MAX];
        let mut seen = 0;
        while seen < count {
            let Some(value) = self
                .client
                .next_value(&mut into, WATCH_WAIT)
                .map_err(text)?
            else {
                continue;
            };
            self.print_value(item, method, &into[..value.len])?;
            seen += 1;
        }
        Ok(())
    }

    fn set(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        if item.kind == ItemKind::Value {
            return Err(format!("{wanted} is read-only"));
        }
        let (contract, method) = method_of(&description, item)?;
        let args = encode_args(&method.params, values)?;
        self.invoke(name, &description, contract, method, &args)?;
        Ok(())
    }

    fn call(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let (contract, method) = find_method(&description, wanted)?;
        let args = encode_args(&method.params, values)?;
        let reply = self.invoke(name, &description, contract, method, &args)?;
        if method.returns != OwnedShape::Unit {
            let value = render_reply(method, &reply)?;
            line(self.out, format_args!("{value}"))?;
        }
        Ok(())
    }

    fn power(&mut self, name: &str, on: bool) -> Outcome {
        let device = self.device(name)?;
        self.client.set_power(device.index, on).map_err(text)
    }
}
