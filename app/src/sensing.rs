mod builtins;

mod sensor;
pub use sensor::{Sensor, LoadError};

use crate::parser::expr::{Type, Value};
use crate::base::AppPath;

#[derive(Debug, Clone, Copy)]
pub struct SensingId {
    high: u16, // 0xFFF* are reserved for special perpose
    low: u16,
}

const SID_HIGH_BUILTIN: u16 = 0xFFFF;
const SID_HIGH_DEBUG: u16 = 0xFFFE;

pub struct Sensors {
    sensors: Vec<Option<(String, Sensor)>>,
    builtins: builtins::BuiltinSensor,
    #[cfg(debug_assertions)]
    debug: builtins::debug::DebugSensor,
}

unsafe impl Send for Sensors {}
unsafe impl Sync for Sensors {}

pub enum SensorPrepareError {
    InvalidFilename,
    DuplicatedName,
    LoadError(sensor::LoadError),
    CouldNotReserve(std::io::Error),
}

impl From<sensor::LoadError> for SensorPrepareError {
    fn from(e: sensor::LoadError) -> Self {
        SensorPrepareError::LoadError(e)
    }
}

impl Sensors {
    pub fn new() -> Self {
        Self {
            sensors: vec![],
            builtins: builtins::BuiltinSensor::create(),
            #[cfg(debug_assertions)]
            debug: builtins::debug::DebugSensor::create(),
        }
    }

    pub fn load(&mut self, path: &AppPath) -> Result<(), SensorPrepareError> {
        let Some(Some(module)) = path.file_prefix().map(|s| s.to_str()) else {
            return Err(SensorPrepareError::InvalidFilename);
        };

        if let Some((_i, _)) = self.sensors.iter().enumerate()
        .filter_map(|(i, x)| x.as_ref().map(|x| (i,x)))
        .find(|(_i, (name, _))| name == module) {
            return Err(SensorPrepareError::DuplicatedName);
        }

        let dst = crate::base::path().templ(path.file_name().unwrap());
        if let Err(e) = std::fs::copy(path, &dst) {
            return Err(SensorPrepareError::CouldNotReserve(e));
        };

        let s = Sensor::new(dst)?;
        self.sensors.push(Some((module.to_string(), s)));

        Ok(())
    }

    pub fn register(&mut self, ident_path: &str) -> Option<(Type, SensingId)> {
        // path is guaranteed to match ([a-zA-Z$][a-zA-Z0-9]*,)*

        #[cfg(debug_assertions)]
        {
            let sid_low = self.debug.register(ident_path);
            if sid_low > 0 {
                return Some((Type::Float, SensingId { high: SID_HIGH_DEBUG, low: sid_low }));
            }
        }

        let (_module, sid_high, sid_low) = 'ba: {
            // search in modules first
            let (module, subpath) = ident_path.split_once('.').unwrap_or((ident_path, ""));
            if let Some((i, (_, sensor))) = self.sensors.iter_mut().enumerate()
            .filter_map(|(i, x)| x.as_mut().map(|x| (i,x)))
            .find(|(_i, (name, _))| &*name == module) {
                assert!(i < 0xFFF0); // there should never be so much modules loaded at once
                let sid_high = i as u16;
                let sid_low = sensor.register(subpath);
                break 'ba (module, sid_high, sid_low);
            }

            // fallback to builtins
            let sid_high = SID_HIGH_BUILTIN;
            let sid_low = self.builtins.register(ident_path);
            ("builtin", sid_high, sid_low)
        };

        if sid_low == 0 { return None; }
        return Some((Type::Float, SensingId {
            high: sid_high,
            low: sid_low,
        }));
    }

    pub fn unregister(&mut self, sid: SensingId) {
        #[cfg(debug_assertions)]
        {
            if sid.high == SID_HIGH_DEBUG {
                self.debug.unregister(sid.low);
                return;
            }
        }

        if sid.high == SID_HIGH_BUILTIN {
            self.builtins.unregister(sid.low);
            return;
        }

        let Some((_, sensor)) = self.sensors[sid.high as usize].as_mut() else {
            log::error!("unregistering from module that is unloaded (sid={:x},{:x})", sid.high, sid.low);
            return;
        };

        sensor.unregister(sid.low)
    }

    pub fn read(&self, sid: SensingId) -> Option<Value> {
        #[cfg(debug_assertions)]
        {
            if sid.high == SID_HIGH_DEBUG {
                return Some(Value { float: self.debug.read(sid.low) })
            }
        }

        let value = if sid.high == SID_HIGH_BUILTIN {
            self.builtins.read(sid.low)
        } else {
            let Some((_, sensor)) = self.sensors[sid.high as usize].as_ref() else {
                log::error!("resolving value from module that is unloaded (sid={:x},{:x})", sid.high, sid.low);
                return None;
            };

            sensor.read(sid.low)
        };

        Some(Value { float: value } )
    }

    pub fn refresh(&mut self) {
        #[cfg(debug_assertions)]
        self.debug.refresh();
        self.builtins.refresh();
        for sensor in &mut self.sensors {
            if let Some((_name, sensor)) = sensor {
                sensor.refresh();
            }
        }
    }
}