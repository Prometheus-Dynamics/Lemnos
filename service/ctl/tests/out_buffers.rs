//! Out-buffer methods through the text path: `lemnos-ctl call` on the simulated board, and every
//! out-buffer method of every contract in `contracts/`, encoded the way the tool sends it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_core::{Contract, ContractInfo, Description, Manifest};
use lemnos_driver_system_slots::SystemSlots;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnos_wire::text::{OUT_LIMIT, encode_args, encode_call};
use lemnos_wire::{
    DeviceDescription, Identity, OwnedMethod, OwnedShape, decode_description, encode_description,
};
use lemnosd::{Config, Service};

/// Every contract in `contracts/`.
const CONTRACTS: &[ContractInfo] = &[
    <dyn lemnos_contract_accelerometer::Accelerometer as Contract>::INFO,
    <dyn lemnos_contract_camera_frames::CameraFrames as Contract>::INFO,
    <dyn lemnos_contract_camera_isp::CameraIsp as Contract>::INFO,
    <dyn lemnos_contract_camera_receiver::CameraReceiver as Contract>::INFO,
    <dyn lemnos_contract_camera_sensor::CameraSensor as Contract>::INFO,
    <dyn lemnos_contract_codec_block::CodecBlock as Contract>::INFO,
    <dyn lemnos_contract_fan_control::FanControl as Contract>::INFO,
    <dyn lemnos_contract_fan::Fan as Contract>::INFO,
    <dyn lemnos_contract_gpu_lease::GpuLease as Contract>::INFO,
    <dyn lemnos_contract_gyroscope::Gyroscope as Contract>::INFO,
    <dyn lemnos_contract_image_blend::ImageBlend as Contract>::INFO,
    <dyn lemnos_contract_image_convert::ImageConvert as Contract>::INFO,
    <dyn lemnos_contract_image_scaler::ImageScaler as Contract>::INFO,
    <dyn lemnos_contract_isp_isp600::IspIsp600 as Contract>::INFO,
    <dyn lemnos_contract_isp_pisp::IspPisp as Contract>::INFO,
    <dyn lemnos_contract_lens::Lens as Contract>::INFO,
    <dyn lemnos_contract_light::Light as Contract>::INFO,
    <dyn lemnos_contract_magnetometer::Magnetometer as Contract>::INFO,
    <dyn lemnos_contract_npu::Npu as Contract>::INFO,
    <dyn lemnos_contract_orientation::Orientation as Contract>::INFO,
    <dyn lemnos_contract_pixels::Pixels as Contract>::INFO,
    <dyn lemnos_contract_power_monitor::PowerMonitor as Contract>::INFO,
    <dyn lemnos_contract_power_switch::PowerSwitch as Contract>::INFO,
    <dyn lemnos_contract_raw_bus::RawBus as Contract>::INFO,
    <dyn lemnos_contract_security::Security as Contract>::INFO,
    <dyn lemnos_contract_security_precondition::SecurityPrecondition as Contract>::INFO,
    <dyn lemnos_contract_system_identity::SystemIdentity as Contract>::INFO,
    <dyn lemnos_contract_system_reboot::Reboot as Contract>::INFO,
    <dyn lemnos_contract_system_slots::Slots as Contract>::INFO,
    <dyn lemnos_contract_temperature::Temperature as Contract>::INFO,
    <dyn lemnos_contract_usb_function::UsbFunction as Contract>::INFO,
];

fn described() -> DeviceDescription {
    let manifest = Manifest {
        name: "walk",
        needs: &[],
        provides: CONTRACTS,
    };
    let mut bytes = vec![0; 256 * 1024];
    let identity = Identity {
        label: "walk",
        bus: "",
        state: 0,
        reason: "",
        not_live: false,
        jobs: "",
    };
    let n = encode_description(&manifest, &Description::new(), &identity, &mut bytes).unwrap();
    decode_description(&bytes[..n]).unwrap()
}

/// One word a shape takes, for each shape there is.
fn sample(shape: &OwnedShape, words: &mut Vec<String>) {
    let word = match shape {
        OwnedShape::Unit => return,
        OwnedShape::Bool => "true",
        OwnedShape::U8
        | OwnedShape::U16
        | OwnedShape::U32
        | OwnedShape::U64
        | OwnedShape::I8
        | OwnedShape::I16
        | OwnedShape::I32
        | OwnedShape::I64 => "1",
        OwnedShape::F32 | OwnedShape::F64 => "1.5",
        OwnedShape::Str => "word",
        OwnedShape::Bytes => "0a",
        OwnedShape::Option(_) => "none",
        OwnedShape::Array(inner, n) => {
            for _ in 0..*n {
                sample(inner, words);
            }
            return;
        }
        OwnedShape::Record(fields) => {
            for (_, field) in fields {
                sample(field, words);
            }
            return;
        }
    };
    words.push(word.to_string());
}

/// The contract sources: `contracts/<dir>/src/lib.rs`, where each contract is one trait.
fn contract_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|entry| entry.unwrap().path().join("src/lib.rs"))
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no contract sources under {}",
        root.display()
    );
    files
}

/// The arguments of `#[contract("name", version = N, kind = "...")]`, as the contract macro reads
/// them: the tag `name/N`. The kind is checked for its shape only.
fn contract_args(input: syn::parse::ParseStream) -> syn::Result<String> {
    let name: syn::LitStr = input.parse()?;
    input.parse::<syn::Token![,]>()?;
    let key: syn::Ident = input.parse()?;
    if key != "version" {
        return Err(syn::Error::new(key.span(), "expected `version = N`"));
    }
    input.parse::<syn::Token![=]>()?;
    let version: syn::LitInt = input.parse()?;
    if input.peek(syn::Token![,]) {
        input.parse::<syn::Token![,]>()?;
    }
    if !input.is_empty() {
        let key: syn::Ident = input.parse()?;
        if key != "kind" {
            return Err(syn::Error::new(key.span(), "expected `kind = \"...\"`"));
        }
        input.parse::<syn::Token![=]>()?;
        input.parse::<syn::LitStr>()?;
        if input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
        }
    }
    Ok(format!(
        "{}/{}",
        name.value(),
        version.base10_parse::<u32>()?
    ))
}

/// The out-buffer methods of the contracts the device set serves, read from their sources: every
/// trait with a `#[contract]` attribute whose tag is in `served`, and each of its methods that has
/// an `#[out]` parameter. The syntax tree has no comments in it, so a doc that names `#[out]`
/// counts for nothing. Returns the tags found, and the `(tag, method)` pairs.
fn source_out_methods(served: &[&str]) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let (mut tags, mut methods) = (BTreeSet::new(), BTreeSet::new());
    for file in contract_sources() {
        let text = std::fs::read_to_string(&file).unwrap();
        let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for item in parsed.items {
            let syn::Item::Trait(contract) = item else {
                continue;
            };
            let Some(attr) = contract
                .attrs
                .iter()
                .find(|a| a.path().is_ident("contract"))
            else {
                continue;
            };
            let tag = attr
                .parse_args_with(contract_args)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            if !served.contains(&tag.as_str()) {
                continue;
            }
            tags.insert(tag.clone());
            for member in &contract.items {
                let syn::TraitItem::Fn(method) = member else {
                    continue;
                };
                let out = method.sig.inputs.iter().any(|arg| {
                    matches!(arg, syn::FnArg::Typed(param)
                        if param.attrs.iter().any(|a| a.path().is_ident("out")))
                });
                if out {
                    methods.insert((tag.clone(), method.sig.ident.to_string()));
                }
            }
        }
    }
    (tags, methods)
}

fn named(pairs: &[&(String, String)]) -> String {
    if pairs.is_empty() {
        return "none".to_string();
    }
    let names: Vec<String> = pairs.iter().map(|(tag, m)| format!("{tag} {m}")).collect();
    names.join(", ")
}

#[test]
fn every_out_buffer_method_of_every_contract_is_callable_through_the_text_path() {
    let description = described();
    assert_eq!(description.contracts.len(), CONTRACTS.len());
    let served: Vec<&str> = CONTRACTS.iter().map(|c| c.tag.as_str()).collect();
    // The expected set comes from the sources, not from a count kept by hand: a method added to a
    // contract with an `#[out]` parameter is expected here with no edit to this test.
    let (tags, expected) = source_out_methods(&served);
    let unread: Vec<&str> = served
        .iter()
        .copied()
        .filter(|tag| !tags.contains(*tag))
        .collect();
    assert!(
        unread.is_empty(),
        "no contract source under contracts/ declares these: {unread:?}"
    );
    let mut walked = BTreeSet::new();
    for contract in &description.contracts {
        for method in contract.methods.iter().filter(|m| m.has_out_buffer) {
            walked.insert((contract.tag.clone(), method.name.clone()));
            check(method);
        }
    }
    // Both ways: a source method whose description lost `has_out_buffer` (so it is never walked),
    // and a described method that no source declares with `#[out]`.
    let lost: Vec<&(String, String)> = expected.difference(&walked).collect();
    let extra: Vec<&(String, String)> = walked.difference(&expected).collect();
    assert!(
        lost.is_empty() && extra.is_empty(),
        "out-buffer methods differ between contracts/ and the device description.\n\
         `#[out]` in the source, not walked (no has_out_buffer in the description): {}\n\
         walked from the description, no `#[out]` in the source: {}",
        named(&lost),
        named(&extra),
    );
}

fn check(method: &OwnedMethod) {
    let mut words = Vec::new();
    for param in &method.params {
        sample(&param.shape, &mut words);
    }
    let fixed = encode_args(&method.params, &words).unwrap();
    // No size word: the default, which is the frame limit.
    let call = encode_call(method, &words).unwrap_or_else(|e| panic!("{}: {e}", method.name));
    assert_eq!(call.out_capacity, Some(OUT_LIMIT), "{}", method.name);
    assert_eq!(&call.args[..fixed.len()], &fixed[..], "{}", method.name);
    assert_eq!(call.args.len(), fixed.len() + 4, "{}", method.name);
    assert_eq!(
        call.args[fixed.len()..],
        OUT_LIMIT.to_le_bytes(),
        "{}",
        method.name
    );
    // A size word after the arguments replaces the default.
    let mut sized = words.clone();
    sized.push("64".to_string());
    let call = encode_call(method, &sized).unwrap();
    assert_eq!(call.out_capacity, Some(64), "{}", method.name);
    assert_eq!(
        call.args[fixed.len()..],
        64u32.to_le_bytes(),
        "{}",
        method.name
    );
    assert_eq!(call.reply_len(), 64);
    // A word too many is refused, not dropped.
    sized.push("64".to_string());
    assert!(encode_call(method, &sized).is_err(), "{}", method.name);
}

#[test]
fn a_method_without_an_out_buffer_takes_no_size_word() {
    for contract in &described().contracts {
        for method in contract.methods.iter().filter(|m| !m.has_out_buffer) {
            let mut words = Vec::new();
            for param in &method.params {
                sample(&param.shape, &mut words);
            }
            let call = encode_call(method, &words).unwrap();
            assert_eq!(call.out_capacity, None, "{}", method.name);
            assert_eq!(call.reply_len(), 0);
            assert_eq!(call.args, encode_args(&method.params, &words).unwrap());
            words.push("1".to_string());
            assert!(encode_call(method, &words).is_err(), "{}", method.name);
        }
    }
}

// The simulated board, with the slot driver over temporary directories.

struct Running {
    _service: Service,
    socket: PathBuf,
}

static NEXT: AtomicU32 = AtomicU32::new(0);

fn start_slots() -> Running {
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-out-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let (data, boot) = (dir.join("data"), dir.join("boot"));
    std::fs::create_dir_all(data.join("slots")).unwrap();
    std::fs::create_dir_all(boot.join("slots")).unwrap();
    let socket = dir.join("sock");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"slots\"\ndriver = \"system-slots\"\n\
         config = {{ data = \"{}\", boot = \"{}\", record = \"file\", partuuid = \"abcd-02\", \
         boot_sources = [\"rpi-boot-mode-1=/soc/mmc@7e202000\"], boot_args = \"os\", reserve_mb = 0, running = \"\" }}\n",
        data.display(),
        boot.display()
    );
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<SystemSlots>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    let running = Running {
        _service: service,
        socket,
    };
    // The device starts after the service does: a call before it is running is refused as not
    // running. Wait for the state the daemon reports, as `lemnos-ctl list` shows it.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let listed = ctl(&running, &["list"]).unwrap();
        if listed
            .lines()
            .any(|row| row.contains("slots") && row.contains("running"))
        {
            break running;
        }
        assert!(
            Instant::now() < deadline,
            "the slots device never ran: {listed}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs the tool, asking again while the device is busy, as the other tool tests do.
/// How long a test waits for a walked device to bind. A device whose bind fails retries after the
/// host's first retry (`retry_first`, 1 s by default), so two of those cover one retry.
const BIND_WAIT: Duration = Duration::from_secs(2);

fn ctl(running: &Running, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let deadline = Instant::now() + BIND_WAIT;
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            // Busy, or not bound yet: the device retries its bind with backoff.
            Err(error)
                if (error.starts_with("refused (-16)") || error.starts_with("refused (-19)"))
                    && Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(String::from_utf8(out).unwrap()),
        }
    }
}

#[test]
fn call_slots_list_prints_the_json_on_the_simulated_board() {
    let running = start_slots();
    let text = ctl(&running, &["call", "slots", "list"]).unwrap();
    assert!(text.starts_with("{\"slot_active\":"), "{text}");
    assert!(text.contains("\"slots\":[]"), "{text}");
    assert!(text.ends_with("}\n"), "{text}");
}

#[test]
fn a_size_that_is_too_small_says_how_to_ask_for_more() {
    let running = start_slots();
    let error = ctl(&running, &["call", "slots", "list", "8"]).unwrap_err();
    assert!(error.contains("ask for more"), "{error}");
    assert!(error.contains("8-byte buffer"), "{error}");
    // The same call with a size that fits prints the reply.
    let text = ctl(&running, &["call", "slots", "list", "65536"]).unwrap();
    assert!(text.starts_with("{\"slot_active\":"), "{text}");
    // A size that is not a number of bytes is refused before a call.
    let bad = ctl(&running, &["call", "slots", "list", "lots"]).unwrap_err();
    assert!(bad.contains("buffer size"), "{bad}");
}
