//! Orientation from an accelerometer, a gyroscope and, if there is one, a
//! magnetometer. Without a magnetometer the device is 6-axis: it still reports
//! roll and pitch, and its yaw is 0.
//!
//! This first version reads tilt from gravity alone. PR B10 brings the fusion
//! filter and calibration across from dev.

#![no_std]

use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;
use lemnos_contract_orientation::Orientation as OrientationContract;
use lemnos_core::{
    Config, Description, Dev, Health, Item, Lifecycle, MethodId, Result, Unit, Vector3, driver,
};

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

#[driver("orientation", provides = [OrientationContract])]
pub struct Orientation {
    #[needs]
    accel: Dev<dyn Accelerometer>,
    #[needs]
    gyro: Dev<dyn Gyroscope>,
    #[needs(optional)]
    mag: Option<Dev<dyn Magnetometer>>,
}

impl Lifecycle for Orientation {
    type Needs = OrientationNeeds;

    fn start(needs: OrientationNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            accel: needs.accel,
            gyro: needs.gyro,
            mag: needs.mag,
        })
    }

    fn describe(&self, out: &mut Description) {
        for (name, field) in [("roll", "x"), ("pitch", "y"), ("yaw", "z")] {
            out.add(Item::value(name, READ).unit(Unit::Radian, 0).field(field));
        }
    }

    fn health(&self) -> Health {
        if self.mag.is_none() {
            return Health::Degraded("no magnetometer: 6-axis");
        }
        Health::Ok
    }
}

impl OrientationContract for Orientation {
    fn read(&mut self) -> Result<Vector3> {
        let down = self.accel.read()?;
        self.gyro.read()?;
        let roll = libm::atan2f(down.y, down.z);
        let pitch = libm::atan2f(-down.x, libm::sqrtf(down.y * down.y + down.z * down.z));
        let yaw = match &mut self.mag {
            Some(mag) => {
                let field = mag.read()?;
                libm::atan2f(-field.y, field.x)
            }
            None => 0.0,
        };
        Ok(Vector3 {
            x: roll,
            y: pitch,
            z: yaw,
        })
    }
}
