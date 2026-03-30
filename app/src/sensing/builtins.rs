#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows as sys;

pub use sys::BuiltinSensor;


#[cfg(debug_assertions)]
pub mod debug {
    // read sensing values from a file

    use crate::parser::{self, toml, WithPos, lineview};
    use crate::base::write_report;

    use std::fs;


    #[derive(Debug)]
    pub struct DebugSensor {
        decls: Vec<(String, bool)>, // name, is_float
        pub data: Vec<f64>,
    }

    impl DebugSensor {
        pub fn nil() -> Self { Self { decls: vec![], data: vec![] } }

        pub fn create() -> Self {
            let path = crate::base::path().plugin("debug.toml");

            let Ok(src) = fs::read_to_string(&path) else {
                log::info!("debug sensor file not found at '{path}'");
                return Self::nil();
            };

            let tbl = match toml::Parser::new(&src).parse() {
                Ok(tbl) => tbl,
                Err(WithPos{ pos, val }) => {
                    let (buf, span) = lineview(&src, pos.span);
                    write_report(|f| val.message_with_evidence(
                        f, &path.as_rel().to_string_lossy(), pos.line, buf, Some(span)
                    ));
                    return Self::nil();
                }
            };

            let (mut decls, mut data) = (vec![], vec![]);
            for entry in tbl.0 {
                if let toml::Value::Number(v) = entry.val.val {
                    decls.push((entry.key.val.to_string(), true));
                    data.push(v);
                    log::info!("debug sensor overwrites sensor '{}' = {}", entry.key.val, v);
                }
                if let toml::Value::Boolean(v) = entry.val.val {
                    decls.push((entry.key.val.to_string(), false));
                    data.push(if v { 1.0 } else { 0.0 });
                    log::info!("debug sensor overwrites sensor '{}' = {}", entry.key.val, v);
                }
            }

            Self { decls, data }
        }

        pub fn refresh(&mut self) {
            // TODO report error
            if self.decls.is_empty() { return; }

            let path = crate::base::path().plugin("debug.toml");

            let Ok(src) = fs::read_to_string(&path) else {
                write_report(|f|
                    write!(f, "debug sensor file could not be read at '{path}'")
                );

                return;
            };

            let tbl = match toml::Parser::new(&src).parse() {
                Ok(tbl) => tbl,
                Err(WithPos{ pos, val }) => {
                    let (buf, span) = lineview(&src, pos.span);
                    write_report(|f| val.message_with_evidence(
                        f, &path.as_rel().to_string_lossy(), pos.line, buf, Some(span)
                    ));
                    return;
                }
            };

            // TODO: they are likely in the order
            for ((name, is_float), val) in self.decls.iter().zip(self.data.iter_mut()) {
                let ret = if *is_float {
                    tbl.retrieve::<f64>(name).map(|v| *v.val)
                } else {
                    tbl.retrieve::<bool>(name).map(|v| if *v.val {1.0} else {0.0})
                };

                match ret {
                    Ok(v) => *val = v,
                    // TODO remove _fieldname field?, the caller would know the field name with
                    Err(WithPos { pos, val: toml::RetrieveError::FieldNotFound(_fieldname) }) => {
                        let (buf, span) = lineview(&src, pos.span);
                        write_report(|f| parser::message_with_evidence(
                            f, log::Level::Error, &path.as_rel().to_string_lossy(), pos.line, buf, Some(span), |f|
                            write!(f, "debug sensor '{name}' has not been found, keeping the last value")
                        ));
                    }
                    Err(WithPos { pos, val: toml::RetrieveError::IncompatibleType(_fieldname, expected, found) }) => {
                        let (buf, span) = lineview(&src, pos.span);
                        write_report(|f| parser::message_with_evidence(
                            f, log::Level::Error, &path.as_rel().to_string_lossy(), pos.line, buf, Some(span), |f|
                            write!(f, "debug sensor '{name}' has {} type but given {}, ignoring new value", expected, found)
                        ));
                    }
                }
            }
        }

        pub fn register(&mut self, path: &str) -> Option<u16> {
            self.decls.iter().position(|(s, _)| &s == &path)
            .map(|i| {
                let is_float = self.decls[i].1;
                i as u16 | if is_float { 0x0000 } else { 0x8000 }
            })
        }

        // // DebugSensor only updates when the file is actually updated.
        // // for every update we read the whole file anyways.
        // // managing rc would not improve performance.
        // pub fn unregister(&mut self, sid: u16) {
        // }

        // pub fn destroy(self) {
        //     // does nothing
        // }
    }
}