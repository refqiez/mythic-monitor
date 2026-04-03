mod builtins;

mod sensor;
pub use sensor::{Sensor, LoadError, OpaqueError, OpaqueErrorOwned, OpaqueErrorMsgFail};

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

        let dst = crate::base::app_paths().templ(path.file_name().unwrap());
        if let Err(e) = std::fs::copy(path, &dst) {
            return Err(SensorPrepareError::CouldNotReserve(e));
        };

        let s = Sensor::new(dst)?;
        self.sensors.push(Some((module.to_string(), s)));

        Ok(())
    }

    pub fn register(&mut self, ident_path: &str) -> Result<(Type, SensingId), (&'_ str, OpaqueError<'_>)> {
        // path is guaranteed to match ([a-zA-Z$][a-zA-Z0-9]*,)*

        #[cfg(debug_assertions)]
        {
            if let Some(sid_low) = self.debug.register(ident_path) {
                let type_ = if sid_low & 0x8000 == 0 { Type::Float } else { Type::Bool };
                let sid = SensingId { high: SID_HIGH_DEBUG, low: sid_low };
                return Ok((type_, sid));
            }
        }

        // search in modules first
        let (module, subpath) = ident_path.split_once('.').unwrap_or((ident_path, ""));

        let (_module, sid_high, sid_low) = {
            if let Some((i, (name, sensor))) =
                self.sensors.iter_mut().enumerate()
                // for loaded modules
                .filter_map(|(i, x)| x.as_mut().map(|x| (i,x)))
                // find one with matching name
                .find(|(_, (name, _))| &*name == module) {
                    // then run register
                    assert!(i < 0xFFF0); // there should never be so much modules loaded at once
                    let sid_high = i as u16;
                    let sid_low = sensor.register(subpath).map_err(|e| (name.as_str(), e))?;
                    (module, sid_high, sid_low)

                // fallback to builtins
            } else {
                let sid_high = SID_HIGH_BUILTIN;
                let sid_low = self.builtins.register(ident_path).map_err(|e| ("builtin", e))?;
                ("builtin", sid_high, sid_low)
            }
        };

        let type_ = if sid_low & 0x8000 == 0 { Type::Float } else { Type::Bool };
        let sid = SensingId {
            high: sid_high,
            low: sid_low,
        };
        Ok((type_, sid))
    }

    pub fn unregister(&mut self, sid: SensingId) -> Result<(), (&str, OpaqueError<'_>)> {
        #[cfg(debug_assertions)]
        {
            if sid.high == SID_HIGH_DEBUG {
                // DebugSensor does nothing on unregister
                // self.debug.unregister(sid.low);
                return Ok(());
            }
        }

        if sid.high == SID_HIGH_BUILTIN {
            return self.builtins.unregister(sid.low).map_err(|e| ("builtin", e));
        }

        let sensors_ptr = &self.sensors as *const _;
        let Some((name, sensor)) = self.sensors[sid.high as usize].as_mut() else {
            // Rust's burrow checker does not allow it since mutable borrow lifetime of self extends
            // after the return of this function (via OpaqueError).
            // Borrowing self.sensors as immutable to print in panic is safe here, since it panics anyways.
            // So, we do small pointer trick to bypass the borrow checker.
            unsafe { panic!("unregistering from module that is unloaded (sid={:x},{:x}), {:?}", sid.high, sid.low, &*sensors_ptr); }
        };

        sensor.unregister(sid.low).map_err(|e| (name.as_str(), e))
    }

    pub fn read(&self, sid: SensingId) -> Value {
        #[cfg(debug_assertions)]
        {
            if sid.high == SID_HIGH_DEBUG {
                let is_float = sid.low & 0x8000 == 0;
                let value = self.debug.data[(sid.low & 0xFF) as usize];
                if is_float {
                    Value { float: value }
                } else {
                    Value { bool: value != 0.0 }
                };
            }
        }

        let value = if sid.high == SID_HIGH_BUILTIN {
            self.builtins.data[(sid.low & 0xFF) as usize]
        } else {
            let Some((_, sensor)) = self.sensors[sid.high as usize].as_ref() else {
                panic!("resolving value from module that is unloaded (sid={:x},{:x}), {:?}", sid.high, sid.low, self.sensors);
            };
            sensor.read(sid.low)
        };

        let is_float = sid.low & 0x8000 == 0;
        return if is_float {
            Value { float: value }
        } else {
            Value { bool: value != 0.0 }
        };
    }

    pub fn refresh(&mut self) -> impl Iterator<Item=(&str, OpaqueError<'_>)> {
        let builtin_err = std::iter::once(self.builtins.refresh())
        .filter_map(|x| x.err().map(|e| ("builtin", e)));

        let other_err = self.sensors.iter_mut()
        .filter_map(|s| s.as_mut())
        .filter_map(|(name, sensor)|
            sensor.refresh().err().map(|e| (name.as_str(), e))
        );

        builtin_err.chain(other_err)
    }

    #[cfg(debug_assertions)]
    pub fn refresh_debug(&mut self) -> Result<(), OpaqueError<'_>> {
        self.debug.refresh(); // for current impl, debug plugin handles error internally
        Ok(())
    }

    pub fn destroy_all(&mut self) -> impl Iterator<Item=(String, OpaqueErrorOwned)> + use<'_> {
        // // they do nothing on destroy, and depends on drop.
        // self.builtins.destroy();
        // self.debug.destroy();

        self.sensors.drain(..)
        .filter_map(|s| s)
        .filter_map(|(name, sensor)|
            sensor.drop().err().map(|e| (name, e))
        )
    }
}
