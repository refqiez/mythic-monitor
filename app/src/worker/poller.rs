use crate::base::{AppPath, AppPathAnalisys, analize_path};
use crate::sensing::Sensors;
use super::watcher::SensingUpdate;

use std::sync::{mpsc, Arc, RwLock};
use std::time::{Instant, Duration};

pub struct SensorPoller {
    update_queue: mpsc::Receiver<SensingUpdate>,
    sensors: Arc<RwLock<Sensors>>,
}

impl SensorPoller {
    pub fn new(update_queue: mpsc::Receiver<SensingUpdate>, sensors: Arc<RwLock<Sensors>>) -> Self {
        Self { update_queue, sensors, }
    }

    pub fn load_all(&mut self) {
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");

        // dfs to find shared libs
        let mut stack = vec![crate::base::path().plugin("")];
        while let Some(path) = stack.pop() {
            let metadata = match std::fs::metadata(&path) {
                Ok(mt) => mt,
                Err(e) => {
                    log::warn!("could not read metadata of {path} ({e}), ignoring");
                    continue;
                }
            };

            if metadata.is_file() {
                use crate::sensing::{SensorPrepareError::*, LoadError::*};
                if let Err(e) = sensors.load(&path) { match e {
                    InvalidFilename =>
                        log::error!("invalid filename for {path}, it should be non-empty utf8 string, ignoring"),

                    DuplicatedName =>
                        log::error!("duplicated module name of {path}, ignoring"),

                    CouldNotReserve(e) =>
                        log::error!("could not reserve plugin file {path} ({e}), ignoring"),

                    LoadError(LibLoading(e)) =>
                        log::error!("plugin '{path}' could not be loaded: {e}"),

                    LoadError(MagicMismatch(magic)) =>
                        log::error!("plugin '{path}' has invalid magic bits: {magic:08x}"),

                    LoadError(MajorVersionMismatch(plugin, host)) =>
                        log::error!("plugin '{path}' major version mismatch: {plugin} (plugin) != {host} (mythic)"),

                    LoadError(MinorVersionMismatch(plugin, host)) =>
                        log::error!("plugin '{path}' minor version mismatch: {plugin} (plugin) > {host} (mythic)"),

                    LoadError(NullVtable) =>
                        log::error!("plugin '{path}' has invalid vtable"),

                    LoadError(NullHandle) =>
                        log::error!("plugin '{path}' couldn't initiate"),

                }} else {
                    log::info!("plugin {path} loaded");
                }
            }

            if metadata.is_dir() {
                let entries = match path.read_dir() {
                    Ok(entries) => entries,
                    Err(e) => {
                        log::warn!("could not read directory content of {path} ({e}), ignoring");
                        continue;
                    }
                };

                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(e) => {
                            log::warn!("error while fetching content of {path} ({e}), skipping");
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
            log::trace!("sensing refresh iteration");

            let next = last + Duration::from_millis(500);
            last = next;
            let now = Instant::now();
            let wait_duration = next.duration_since(now);

            match self.update_queue.recv_timeout(wait_duration) {
                Ok(_upd) => {
                }

                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let mut sensors = self.sensors.write().expect("poisoned sensors lock");
                    sensors.refresh();
                }

                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        log::info!("SensorPoller exiting");
    }
}