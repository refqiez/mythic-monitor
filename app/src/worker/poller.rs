use crate::base::{AppPath, AppPathAnalisys, analize_path, write_report};
use crate::sensing::{SensorPrepareError, Sensors, OpaqueError, OpaqueErrorMsgFail};
use super::watcher::SensingUpdate;

use std::sync::{mpsc, Arc, RwLock};
use std::time::{Instant, Duration};
use hashbrown::HashSet;
use hashbrown::hash_map::VacantEntry;

pub struct SensorPoller {
    update_queue: mpsc::Receiver<SensingUpdate>,
    sensors: Arc<RwLock<Sensors>>,
    // prevents repeated error messages of same kind
    error_reported: HashSet<ErrKey>, // module name, errcode
}

#[derive(Hash, PartialEq, Eq)]
struct ErrKey {
    name: String,
    errcode: u32,
}

impl hashbrown::Equivalent<ErrKey> for (&str, u32) {
    fn equivalent(&self, key: &ErrKey) -> bool {
        self.0 == key.name && self.1 == key.errcode
    }
}

fn report_sensor_prepare_error(path: &AppPath, e: SensorPrepareError) {
    use crate::sensing::{SensorPrepareError::*, LoadError::*};
    match e {
        InvalidFilename =>
            write_report(|f| write!(f, "invalid filename for '{path}', it should be non-empty utf8 string, ignoring")),

        DuplicatedName =>
            write_report(|f| write!(f, "duplicated module name of '{path}', ignoring")),

        CouldNotReserve(e) =>
            write_report(|f| write!(f, "could not reserve plugin file '{path}' ({e}), ignoring")),

        LoadError(LibLoading(e)) =>
            write_report(|f| write!(f, "plugin '{path}' could not be loaded: {e}")),

        LoadError(MagicMismatch(magic)) =>
            write_report(|f| write!(f, "plugin '{path}' has invalid magic bits: {magic:08x}")),

        LoadError(MajorVersionMismatch(plugin, host)) =>
            write_report(|f| write!(f, "plugin '{path}' has major version mismatch: {plugin} (plugin) != {host} (mythic)")),

        LoadError(MinorVersionMismatch(plugin, host)) =>
            write_report(|f| write!(f, "plugin '{path}' has minor version mismatch: {plugin} (plugin) > {host} (mythic)")),

        LoadError(NullVtable) =>
            write_report(|f| write!(f, "plugin '{path}' has invalid vtable")),

        LoadError(NullHandle) =>
            write_report(|f| write!(f, "plugin '{path}' couldn't initiate")),

        LoadError(Opaque(e)) =>
            report_opaque_error(&path.to_string_lossy(), "loading", e.as_ref()),

    }
}

pub fn report_opaque_error(name: &str, task: &str, e: OpaqueError) {
    let report = |message: std::fmt::Arguments| {
        write_report(|f| write!(f, "plugin {name} reported error during {task}: [{}] {message}; subsequent errors of this type are silenced", e.errcode))
    };

    match e.message {
        Ok(s) => report(format_args!("{}", s)),
        Err(OpaqueErrorMsgFail::Error(e)) => report(format_args!("<ERR {e}>")),
        Err(OpaqueErrorMsgFail::Null) => report(format_args!("<NULL>")),
        Err(OpaqueErrorMsgFail::NotUtf8(s)) =>
            report(format_args!("{s}{}..<UTF8 ERR>", char::REPLACEMENT_CHARACTER)),
    }
}

impl SensorPoller {
    fn report_opaque_error(&self, name: &str, task: &str, e: OpaqueError) -> Option<u32> {
        let key = (name, e.errcode);
        if self.error_reported.get(&key).is_none() {
            let errcode = e.errcode;
            report_opaque_error(name, task, e);
            Some(errcode)
        } else { None }
    }

    pub fn new(update_queue: mpsc::Receiver<SensingUpdate>, sensors: Arc<RwLock<Sensors>>) -> Self {
        Self { update_queue, sensors, error_reported: HashSet::new() }
    }

    pub fn load_all(&mut self) {
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");

        // dfs to find shared libs
        let mut stack = vec![crate::base::path().plugin.clone()];
        while let Some(path) = stack.pop() {
            let metadata = match std::fs::metadata(&path) {
                Ok(mt) => mt,
                Err(e) => {
                    log::warn!("could not read metadata of '{path}' ({e}), ignoring");
                    continue;
                }
            };

            if metadata.is_file() {
                if let Err(e) = sensors.load(&path) {
                    report_sensor_prepare_error(&path, e);
                } else {
                    log::info!("plugin '{path}' loaded");
                }
            }

            if metadata.is_dir() {
                let entries = match path.read_dir() {
                    Ok(entries) => entries,
                    Err(e) => {
                        log::warn!("could not read directory content of '{path}' ({e}), ignoring");
                        continue;
                    }
                };

                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(e) => {
                            log::warn!("error while fetching content of '{path}' ({e}), skipping");
                            continue;
                        }
                    };

                    let path = AppPath::try_from(entry.path()).unwrap();
                    if let AppPathAnalisys::Plugin(_) = analize_path(&path) {
                        stack.push(path);
                    }
                }
            }
        }
    }

    pub fn run(&mut self) {
        log::info!("SensorPoller started running");

        let mut last = Instant::now();
        loop {
            // log::trace!("sensing refresh iteration");

            let next = last + Duration::from_millis(500);
            last = next;
            let now = Instant::now();
            let wait_duration = next.duration_since(now);

            match self.update_queue.recv_timeout(wait_duration) {
                Ok(upd) => {
                    match upd {
                        #[cfg(debug_assertions)]
                        SensingUpdate::PluginDebugUpdage => {
                            let mut sensors = self.sensors.write().expect("poisoned sensors lock");
                            if let Err(e) = sensors.refresh_debug() {
                                if let Some(errcode) = self.report_opaque_error("debug", "refresh", e) {
                                    self.error_reported.insert(ErrKey { name: "debug".to_string(), errcode });
                                }
                            }
                        }
                    }
                }

                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let mut sensors = self.sensors.write().expect("poisoned sensors lock");
                    for (name, e) in sensors.refresh() {
                        if let Some(errcode) = self.report_opaque_error(name, "refresh", e) {
                            self.error_reported.insert(ErrKey { name: name.to_string(), errcode });
                        }
                    }
                }

                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        log::info!("SensorPoller exiting");
    }
}