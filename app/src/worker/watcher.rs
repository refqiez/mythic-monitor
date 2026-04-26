use crate::base::{AppPath, analize_path, app_paths, AppPathAnalisys};
use crate::sensing::Sensors;
use crate::sprites::{ClipBank, Sprites};
use super::{AnimatorUpdate, SensingUpdate, DecoderUpdate};

use std::collections::HashSet;
use std::sync::{mpsc, Arc, RwLock};
use std::time::Duration;
use notify::{self, Watcher};

pub struct DirectoryWatcher {
    animator_update_queue: mpsc::Sender<AnimatorUpdate>,
    sensing_update_queue: mpsc::Sender<SensingUpdate>,
    decoder_update_queue: mpsc::Sender<DecoderUpdate>,

    sprites: Arc<RwLock<Sprites>>,
    sensors: Arc<RwLock<Sensors>>,
    clipbank: Arc<RwLock<ClipBank>>,
}

enum FileUpdateHandleResult {
    Good,
    Exit,
    Restart,
    Pending,
}

impl DirectoryWatcher {
    pub fn new(
        animator_update_queue: mpsc::Sender<AnimatorUpdate>,
        sensing_update_queue: mpsc::Sender<SensingUpdate>,
        decoder_update_queue: mpsc::Sender<DecoderUpdate>,
        sprites: Arc<RwLock<Sprites>>,
        sensors: Arc<RwLock<Sensors>>,
        clipbank: Arc<RwLock<ClipBank>>,
    ) -> Self {
        Self { animator_update_queue, sensing_update_queue, decoder_update_queue, sprites, sensors, clipbank }
    }

    // returns if should be restarted
    pub fn run(&mut self) -> bool {
        log::info!("DirectoryWatcher started running");

        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).unwrap();

        let sprite_path = &app_paths().sprite;
        if let Err(e) = watcher.watch(sprite_path, notify::RecursiveMode::Recursive) {
            log::error!("could not install notify watcher for '{sprite_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{sprite_path}'");
        }

        let plugin_path = &app_paths().plugin;
        if let Err(e) = watcher.watch(plugin_path, notify::RecursiveMode::Recursive) {
            log::info!("could not install notify watcher for '{plugin_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{plugin_path}'");
        }

        let running_path = &app_paths().running;
        if let Err(e) = watcher.watch(running_path, notify::RecursiveMode::NonRecursive) {
            log::info!("could not install notify watcher for '{running_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{running_path}'");
        }

        let mut dirty: HashSet<AppPath> = HashSet::new();
        let mut still_dirty = vec![];
        let debounce_duration = Duration::from_millis(200);

        fn filter_notify_event(mut event: notify::Event) -> Option<AppPath> {
            match event.kind { // ignore errors
                notify::EventKind::Create(notify::event::CreateKind::File) |
                notify::EventKind::Create(notify::event::CreateKind::Other) |
                notify::EventKind::Create(notify::event::CreateKind::Any) |
                notify::EventKind::Modify(notify::event::ModifyKind::Data(_)) |
                notify::EventKind::Modify(notify::event::ModifyKind::Any) |
                notify::EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::To)) => {
                    assert_eq!(event.paths.len(), 1);
                    Some(AppPath::try_from(event.paths.pop().unwrap()).unwrap())
                }
                notify::EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::Both)) => {
                    assert_eq!(event.paths.len(), 2);
                    Some(AppPath::try_from(event.paths.pop().unwrap()).unwrap())
                }
                notify::EventKind::Remove(_) => {
                    assert_eq!(event.paths.len(), 1);
                    let path = AppPath::try_from(event.paths.pop().unwrap()).unwrap();
                    // TODO is there a way to reuse this result in handle_file_reload?
                    if analize_path(&path) == AppPathAnalisys::Running {
                        Some(path)
                    } else { None }
                }
                _ => {
                    log::trace!("ignoring inotify event {event:?}");
                    None
                }
            }
        }

        // using per-watcher debounce; rather than per-file debounce
        let ret = 'outer: loop {
            if still_dirty.is_empty() {
                // wait for next event to happen
                if let Ok(Ok(res)) = rx.recv() {
                    if let Some(path) = filter_notify_event(res) {
                        log::trace!("got watcher event (wake) '{path}'");
                        dirty.insert(path);
                    }
                }
            } else {
                dirty.extend(still_dirty.drain(..));
                log::trace!("continueing with still_dirty files");
            }

            // keep collecting till there's no updates for a duration
            while let Ok(Ok(res)) = rx.recv_timeout(debounce_duration) {
                if let Some(path) = filter_notify_event(res) {
                    log::trace!("got watcher event (following) '{path}'");
                    dirty.insert(path);
                }
            }
            // NOTE: in unlikely cases when the directory is constantly modified, they will not be processed till end of modification.

            // process the updated files
            for path in dirty.drain() {
                log::trace!("processing updated file '{path}'");
                use FileUpdateHandleResult::*;
                match self.handle_file_update(&path) {
                    Good => (),
                    Exit => break 'outer false,
                    Restart => break 'outer true,
                    Pending => still_dirty.push(path),
                }
            }

            // exiting the function, drop the object to shut down other threads
        };

        log::info!("DirectoryWatcher exiting");
        ret
    }

    // 0: keep_going, 1: exit, 2: restart
    fn handle_file_update(&mut self, path: &AppPath) -> FileUpdateHandleResult {
        use FileUpdateHandleResult::*;
        match analize_path(path) {
            AppPathAnalisys::Clip(_)    => { self.reload_webp(path); Good }
            AppPathAnalisys::SpriteList => if self.reload_list_toml(path) { Good } else { Pending }
            AppPathAnalisys::Sprite(_)  => if self.reload_sprite_toml(path) { Good } else { Pending }

            // halt if not exist
            AppPathAnalisys::Running    => if app_paths().running.exists() { Good } else { Exit }

            #[cfg(debug_assertions)]
            AppPathAnalisys::PluginDebug => {
                _ = self.sensing_update_queue.send(SensingUpdate::PluginDebugUpdage);
                Good
            }

            AppPathAnalisys::Plugin(_)  => Restart,

            AppPathAnalisys::TempL(_)   => unreachable!(),
            AppPathAnalisys::Log        => unreachable!(),
            AppPathAnalisys::Lock       => unreachable!(),
            AppPathAnalisys::Unknown    => {
                log::info!("ignoring updates from '{path}'");
                Good
            }
        }
    }

    pub fn reload_list_toml(&mut self, path: &AppPath) -> bool {
        log::info!("reloading list_toml at '{path}'");
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank= self.clipbank.write().expect("poisoned clipbank lock");
        let ret = sprites.reload(path, &mut sensors, &mut clipbank);
        self.animator_update_queue.send(AnimatorUpdate::UpdateQueued);
        ret
    }

    pub fn reload_sprite_toml(&mut self, path: &AppPath) -> bool {
        log::info!("reloading sprite_toml with '{path}'");
        // FIXME this may result in no updates, but we still acquires lock.
        // this may unnecessarily stagger the watcher if there is many un-tracked toml files are added to the directory
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank= self.clipbank.write().expect("poisoned clipbank lock");
        let ret = sprites.reload_sprite(path, &mut sensors, &mut clipbank);
        self.animator_update_queue.send(AnimatorUpdate::UpdateQueued);
        ret
    }

    pub fn reload_webp(&mut self, path: &AppPath) {
        // refresh clips in the clipbank
        let sprites = self.sprites.read().expect("poisoned sprites lock");
        let mut clipbank = self.clipbank.write().expect("poisoned clipbank lock");
        // FIXME this may result in no updates, but we still acquires lock.
        // this may unnecessarily stagger the watcher if there is many un-tracked image files are added to the directory
        match sprites.reload_clip(path, &mut clipbank) {
            Err(e) =>
                log::error!("could not reload clip from '{path}' ({e:?})"), // FIXME more graceful error report?
            Ok(true) => {
                _ = self.decoder_update_queue.send(DecoderUpdate::Rescan);
            }
            Ok(false) => (), // no-op
        }
    }

}