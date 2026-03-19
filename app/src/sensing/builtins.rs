#[cfg(target_os = "windows")]
mod pdh;
#[cfg(target_os = "windows")]
pub use pdh:: BuiltinSensor;


#[cfg(debug_assertions)]
pub mod debug {
    // read sensing values from a file

    use crate::parser::{toml, WithPos, lineview};
    use crate::base::write_report;

    use std::fs;


    #[derive(Debug)]
    pub struct DebugSensor {
        arr: Vec<(String, f32)>,
    }

    impl DebugSensor {
        pub fn nil() -> Self { Self { arr: vec![] } }

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

            let mut arr = vec![];
            for entry in tbl.0 {
                if let toml::Value::Number(v) = entry.val.val {
                    arr.push((entry.key.val.to_string(), v));
                    log::info!("debug sensor overwrites '{}' = {}", entry.key.val, v);
                }
            }

            Self { arr }
        }

        pub fn refresh(&mut self) {
            if self.arr.is_empty() { return; }

            let path = crate::base::path().plugin("debug.toml");

            let Ok(src) = fs::read_to_string(&path) else {
                log::info!("debug sensor file not found at '{path}'");
                self.arr = vec![];
                return;
            };

            let tbl = match toml::Parser::new(&src).parse() {
                Ok(tbl) => tbl,
                Err(WithPos{ pos, val }) => {
                    let (buf, span) = lineview(&src, pos.span);
                    write_report(|f| val.message_with_evidence(
                        f, &path.as_rel().to_string_lossy(), pos.line, buf, Some(span)
                    ));
                    self.arr = vec![];
                    return;
                }
            };

            for (name, val) in self.arr.iter_mut() {
                if let Some(entry) = tbl.get(name) {
                    if let toml::Value::Number(n) = entry.val.val {
                        *val = n;
                    }
                }
            }
        }

        pub fn read(&self, sid: u16) -> f32 {
            self.arr[sid as usize - 1].1
        }

        pub fn register(&mut self, path: &str) -> u16 {
            self.arr.iter().position(|(s,v)| &s == &path)
            .map(|idx| idx + 1)
            .unwrap_or(0) as u16
        }

        pub fn unregister(&mut self, sid: u16) {
        }
    }
}