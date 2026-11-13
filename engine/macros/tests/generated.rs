//! Runs the generated code: a driver served through a client, as a host would.

use lemnos_core::{
    Bound, Caller, Config, Contract, Description, Dev, Driver, I2c, Item, JobId, JobState,
    Lifecycle, LocalCaller, Manifest, MethodId, NeedKind, Provider, Reply, Resources, Result,
    Shape, Status, Tag, Unit, contract, driver,
};

#[derive(lemnos_core::Encode, lemnos_core::Decode, Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub level: f32,
    pub count: u32,
}

#[contract("example.thing", version = 2)]
pub trait Thing {
    #[method(7)]
    fn read(&mut self) -> Result<Sample>;
    #[method(3)]
    fn add(&mut self, a: u32, b: u32) -> Result<u32>;
    #[method(9)]
    fn name_len(&mut self, name: &str, payload: &[u8]) -> Result<u32>;
    #[method(4)]
    fn dump(&mut self, seed: u8, #[out] into: &mut [u8]) -> Result<usize>;
}

#[contract("example.other", version = 1)]
pub trait Other {
    #[method(1)]
    fn ping(&mut self) -> Result<()>;
    #[method(2, job)]
    fn count_to(&mut self, to: u32) -> Result<JobId>;
}

#[driver("example", provides = [Thing, Other])]
pub struct Example {
    count: u32,
    /// The one job: how far it has counted, and to what.
    counting: Option<(u32, u32)>,
    #[needs(optional)]
    bus: Option<I2c>,
    #[needs(optional)]
    peer: Option<Dev<dyn Thing>>,
}

impl Lifecycle for Example {
    type Needs = ExampleNeeds;
    const JOBS: bool = true;

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        match self.counting {
            Some((done, to)) if job == JobId(1) && done < to => {
                self.counting = Some((done + 1, to));
                JobState::Running {
                    done: u64::from(done),
                    total: u64::from(to),
                }
            }
            Some((done, _)) if job == JobId(1) => {
                self.counting = None;
                match lemnos_core::Encode::encode(&done, result) {
                    Ok(()) => JobState::Finished(Status::OK),
                    Err(status) => JobState::Finished(status),
                }
            }
            _ => JobState::Finished(Status::NOT_FOUND),
        }
    }

    fn start(needs: ExampleNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            count: cfg.int("count").unwrap_or(5) as u32,
            counting: None,
            bus: needs.bus,
            peer: needs.peer,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(
            Item::value(
                "level",
                MethodId {
                    contract: 0,
                    method: 7,
                },
            )
            .unit(Unit::Volt, 0)
            .field("level"),
        );
    }
}

impl Thing for Example {
    fn read(&mut self) -> Result<Sample> {
        let _ = (&self.bus, &self.peer);
        Ok(Sample {
            level: 1.5,
            count: self.count,
        })
    }

    fn add(&mut self, a: u32, b: u32) -> Result<u32> {
        a.checked_add(b).ok_or(Status::INVALID_ARGUMENT)
    }

    fn name_len(&mut self, name: &str, payload: &[u8]) -> Result<u32> {
        Ok((name.len() + payload.len()) as u32)
    }

    fn dump(&mut self, seed: u8, into: &mut [u8]) -> Result<usize> {
        into.fill(seed);
        Ok(into.len().min(3))
    }
}

impl Other for Example {
    fn ping(&mut self) -> Result<()> {
        Ok(())
    }

    fn count_to(&mut self, to: u32) -> Result<JobId> {
        self.counting = Some((0, to));
        Ok(JobId(1))
    }
}

struct Nothing;

impl Provider for Nothing {
    fn provided(&self, _need: &str) -> bool {
        false
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }
}

/// Reaches the driver the way a host does: bytes in, bytes out.
struct Direct<'a> {
    driver: &'a mut Example,
    contract: u8,
    args: [u8; 128],
}

impl Caller for Direct<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let mut writer = Reply::new(reply);
        let id = MethodId {
            contract: self.contract,
            method,
        };
        let status = self
            .driver
            .dispatch(id, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

fn bound() -> Example {
    let mut nothing = Nothing;
    let mut res = Resources::new(&mut nothing, None);
    Example::bind(&mut res, &Config::default()).unwrap()
}

fn client(driver: &mut Example) -> Bound<dyn Thing, Direct<'_>> {
    Bound::new(Direct {
        driver,
        contract: 0,
        args: [0; 128],
    })
}

#[test]
fn the_manifest_comes_from_the_struct() {
    const M: Manifest = Example::MANIFEST;
    assert_eq!(M.name, "example");
    let names: Vec<_> = M.needs.iter().map(|n| n.name).collect();
    assert_eq!(names, ["bus", "peer"]);
    assert!(M.needs.iter().all(|n| n.optional));
    assert_eq!(
        M.needs[1].kind,
        NeedKind::Device(Tag::new("example.thing/2"))
    );
    assert_eq!(M.provides[0].tag.as_str(), "example.thing/2");
    assert_eq!(M.provides[1].tag.as_str(), "example.other/1");
}

#[test]
fn methods_keep_their_declared_numbers_and_shapes() {
    let info = <dyn Thing as Contract>::INFO;
    let ids: Vec<_> = info.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(ids, [("read", 7), ("add", 3), ("name_len", 9), ("dump", 4)]);
    assert_eq!(info.methods[1].params[1].name, "b");
    assert_eq!(info.methods[1].params[1].shape, Shape::U32);
    assert!(info.methods[3].has_out_buffer);
    let Shape::Record(fields) = info.methods[0].returns else {
        panic!("a struct is a record");
    };
    assert_eq!(fields[0].name, "level");
    assert_eq!(fields[1].shape, Shape::U32);
}

#[test]
fn a_client_reaches_the_driver() {
    let mut driver = bound();
    let mut thing = client(&mut driver);
    assert_eq!(
        thing.read(),
        Ok(Sample {
            level: 1.5,
            count: 5,
        })
    );
    assert_eq!(thing.add(2, 3), Ok(5));
    assert_eq!(thing.add(u32::MAX, 1), Err(Status::INVALID_ARGUMENT));
    assert_eq!(thing.name_len("abc", &[1, 2]), Ok(5));
}

#[test]
fn an_out_buffer_is_filled_in_place() {
    let mut driver = bound();
    let mut thing = client(&mut driver);
    let mut into = [0; 8];
    assert_eq!(thing.dump(9, &mut into), Ok(3));
    assert_eq!(into, [9; 8]);
}

#[test]
fn an_unknown_method_or_contract_is_not_supported() {
    let mut driver = bound();
    let mut buf = [0; 8];
    for (contract, method) in [(0, 99), (2, 1)] {
        let mut reply = Reply::new(&mut buf);
        let id = MethodId { contract, method };
        assert_eq!(driver.dispatch(id, &[], &mut reply), Status::NOT_SUPPORTED);
    }
}

#[test]
fn short_arguments_are_refused() {
    let mut driver = bound();
    let mut buf = [0; 8];
    let mut reply = Reply::new(&mut buf);
    let add = MethodId {
        contract: 0,
        method: 3,
    };
    assert_eq!(
        driver.dispatch(add, &[1, 0], &mut reply),
        Status::INVALID_ARGUMENT
    );
}

#[test]
fn the_description_comes_from_lifecycle() {
    let driver = bound();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    assert_eq!(out.find("level").unwrap().field, Some("level"));
}

#[test]
fn a_job_method_is_marked_and_its_driver_answers_for_the_job() {
    const M: Manifest = Example::MANIFEST;
    const { assert!(<Example as Driver>::JOBS) };
    let other = M.provides[1].methods;
    assert_eq!(
        other.iter().map(|m| (m.name, m.job)).collect::<Vec<_>>(),
        [("ping", false), ("count_to", true)]
    );
    assert!(<dyn Thing as Contract>::INFO.methods.iter().all(|m| !m.job));

    let mut driver = bound();
    let mut buf = [0; 8];
    let mut reply = Reply::new(&mut buf);
    let start = MethodId {
        contract: 1,
        method: 2,
    };
    assert_eq!(
        driver.dispatch(start, &2u32.to_le_bytes(), &mut reply),
        Status::OK
    );
    assert_eq!(reply.len(), 8);
    assert_eq!(buf, 1u64.to_le_bytes());
    let mut states = Vec::new();
    loop {
        let mut out = [0; 8];
        let mut result = Reply::new(&mut out);
        let state = Driver::job(&mut driver, JobId(1), &mut result);
        let len = result.len();
        states.push((state, out[..len].to_vec()));
        if matches!(state, JobState::Finished(_)) {
            break;
        }
    }
    assert_eq!(
        states,
        [
            (JobState::Running { done: 0, total: 2 }, vec![]),
            (JobState::Running { done: 1, total: 2 }, vec![]),
            (JobState::Finished(Status::OK), 2u32.to_le_bytes().to_vec()),
        ]
    );
    let mut out = [0; 8];
    assert_eq!(
        Driver::job(&mut driver, JobId(1), &mut Reply::new(&mut out)),
        JobState::Finished(Status::NOT_FOUND)
    );
}
