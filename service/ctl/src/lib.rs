//! `lemnos-ctl`: everything it knows about a device comes from that device's
//! description, so it works on devices nobody has written tooling for.

use std::io::Write;
use std::time::Duration;

use lemnos_client::{Client, DeviceEntry, DeviceHealth, DeviceState, Error, PackageMode, follow};
use lemnos_core::{ItemKind, Status};
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
  call DEVICE METHOD [ARG...]    any method, as NAME or TAG:NAME; a job method runs as a
                                 long job, its progress printed until it ends (interrupting
                                 the tool cancels it)
  off DEVICE | on DEVICE
  packages                       driver packages: which loaded, which were refused and why
  validate [--socs DIR] [--no-live] BOARD
                                 check a board file and show the host settings it gives, with no
                                 daemon; with a `soc`, also check the live device tree against it
  tree [--socs DIR | --map FILE] BOARD [-o FILE] [--boot FILE]
                                 write the device-tree overlay for a board (stdout by default),
                                 and its firmware boot fragment, with no daemon";

const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
const REPLY_MAX: usize = 64 * 1024;
const WATCH_WAIT: Duration = Duration::from_secs(2);
/// How long `call` waits for a long job between looks at its progress. It waits as long as
/// the job runs.
const JOB_WAIT: Duration = Duration::from_secs(1);

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
            [board] => validate(board, None, true, out),
            [flag, dir, board] if flag == "--socs" => validate(board, Some(dir), true, out),
            [flag, board] if flag == "--no-live" => validate(board, None, false, out),
            [flag, dir, nolive, board] if flag == "--socs" && nolive == "--no-live" => {
                validate(board, Some(dir), false, out)
            }
            _ => Err(USAGE.to_string()),
        };
    }
    if command == "tree" {
        return tree(tail, out);
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
fn validate(path: &str, socs: Option<&str>, live: bool, out: &mut dyn Write) -> Outcome {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    // A link names a device of this board, and not the device itself. Whether the two ends agree
    // is only known to a running host, which checks it at bind.
    for device in &board.devices {
        let Some(link) = &device.link else { continue };
        if link == &device.id {
            return Err(format!(
                "{path}: device {:?} is linked to itself",
                device.id
            ));
        }
        if !board.devices.iter().any(|d| &d.id == link) {
            return Err(format!(
                "{path}: device {:?} is linked to {link:?}, which the board doesn't have",
                device.id
            ));
        }
    }
    let problems = match (&board.board.soc, live) {
        (Some(_), true) => live_problems(&board, socs)?,
        _ => Vec::new(),
    };
    validate_settings(path, &board, out)?;
    if !problems.is_empty() {
        for problem in &problems {
            line(out, format_args!("live: {problem}"))?;
        }
        return Err(format!(
            "{path}: {} problems with the live system",
            problems.len()
        ));
    }
    Ok(())
}

/// The problems the live device tree and platform devices have against the board's hardware.
fn live_problems(
    board: &lemnos_host::board::BoardFile,
    socs: Option<&str>,
) -> Result<Vec<String>, String> {
    let map = load_map(board, socs)?;
    Ok(lemnos_devtree::check(
        board,
        &map,
        &lemnos_devtree::check::Live {
            tree: std::path::Path::new("/proc/device-tree"),
            sys: std::path::Path::new("/sys/bus/platform/devices"),
        },
    ))
}

/// The SoC map a board names, read from `socs` (`socs/<soc>.toml`, the repository's folder by
/// default).
fn load_map(
    board: &lemnos_host::board::BoardFile,
    socs: Option<&str>,
) -> Result<lemnos_devtree::SocMap, String> {
    let soc = board.board.soc.as_deref().ok_or("[board] soc is not set")?;
    let dir = socs.unwrap_or("socs");
    let path = format!("{dir}/{soc}.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    lemnos_devtree::SocMap::parse(&text).map_err(|e| format!("{path}: {e}"))
}

/// `tree BOARD [-o FILE] [--boot FILE]`: the overlay and the boot fragment for a board.
fn tree(args: &[String], out: &mut dyn Write) -> Outcome {
    let mut socs = None;
    let mut map_file = None;
    let mut overlay_out = None;
    let mut boot_out = None;
    let mut board_path = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].as_str(), value) {
            ("--socs", Some(dir)) => socs = Some(dir),
            ("--map", Some(file)) => map_file = Some(file),
            ("-o", Some(file)) => overlay_out = Some(file),
            ("--boot", Some(file)) => boot_out = Some(file),
            (flag, _) if board_path.is_none() && !flag.starts_with('-') => {
                board_path = Some(flag.to_string());
                i += 1;
                continue;
            }
            _ => return Err(USAGE.to_string()),
        }
        i += 2;
    }
    let path = board_path.ok_or_else(|| USAGE.to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let board = lemnos_host::board::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    let map = match map_file {
        Some(file) => {
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
            lemnos_devtree::SocMap::parse(&text).map_err(|e| format!("{file}: {e}"))?
        }
        None => load_map(&board, socs.as_deref())?,
    };
    let generated = lemnos_devtree::generate(&board, &map).map_err(|e| format!("{path}: {e}"))?;
    match overlay_out {
        Some(file) => {
            std::fs::write(&file, &generated.overlay).map_err(|e| format!("{file}: {e}"))?
        }
        None => line(out, format_args!("{}", generated.overlay.trim_end()))?,
    }
    if let Some(file) = boot_out {
        std::fs::write(&file, &generated.boot).map_err(|e| format!("{file}: {e}"))?;
    }
    Ok(())
}

fn validate_settings(
    path: &str,
    board: &lemnos_host::board::BoardFile,
    out: &mut dyn Write,
) -> Outcome {
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
            let mut row = format!(
                "{:width$}  {}  {}  {}  {tags}",
                d.name,
                d.driver,
                state_name(d.state),
                health_name(d.health),
            );
            // Why it is not ok, as the host says it: a link that is not confirmed, for one.
            if !d.reason.is_empty() {
                row.push_str(&format!("  ({})", d.reason));
            }
            line(self.out, format_args!("{row}"))?;
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
                let returns = if method.job {
                    "job".to_string()
                } else {
                    shape_name(&method.returns)
                };
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

    /// Runs a job method as a long job: prints its number, its progress as it comes, and its
    /// result in hex, and fails if it ends with anything but success.
    fn job(
        &mut self,
        device: &str,
        description: &DeviceDescription,
        contract: usize,
        method: &OwnedMethod,
        args: &[u8],
    ) -> Outcome {
        let tag = &description.contracts[contract].tag;
        let mut handle = self.client.open_tag(device, tag).map_err(text)?;
        let job = self
            .client
            .start_job(&mut handle, method.id, args)
            .map_err(text)?;
        line(self.out, format_args!("job {job:#x} started"))?;
        loop {
            let mut shown = Ok(());
            let out = &mut *self.out;
            let ended = self.client.wait_job(job, JOB_WAIT, &mut |done, total| {
                if shown.is_ok() {
                    shown = if total == 0 {
                        line(out, format_args!("progress {done}"))
                    } else {
                        line(out, format_args!("progress {done}/{total}"))
                    };
                }
            });
            shown?;
            let Some((status, result)) = ended else {
                continue;
            };
            if !status.is_ok() {
                let how = match status {
                    Status::CANCELLED => " (cancelled)",
                    Status::INTERRUPTED => " (interrupted)",
                    _ => "",
                };
                return Err(format!("job {job:#x} failed: status {}{how}", status.0));
            }
            let hex = result
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            return if hex.is_empty() {
                line(self.out, format_args!("job {job:#x} done"))
            } else {
                line(self.out, format_args!("job {job:#x} done: {hex}"))
            };
        }
    }

    fn set(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let item = find_item(&description, wanted)?;
        if item.kind == ItemKind::Value {
            return Err(format!("{wanted} is read-only"));
        }
        let (contract, method) = method_of(&description, item)?;
        let args = encode_args(&method.params, values)?;
        if method.job {
            return self.job(name, &description, contract, method, &args);
        }
        self.invoke(name, &description, contract, method, &args)?;
        Ok(())
    }

    fn call(&mut self, name: &str, wanted: &str, values: &[String]) -> Outcome {
        let (_, description) = self.description(name)?;
        let (contract, method) = find_method(&description, wanted)?;
        let args = encode_args(&method.params, values)?;
        if method.job {
            return self.job(name, &description, contract, method, &args);
        }
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
