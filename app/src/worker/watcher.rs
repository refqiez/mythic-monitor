use crate::base::{AppPath, analize_path, AppPathAnalisys};
use crate::sensing::Sensors;
use crate::sprites::{ClipBank, SpriteId, Sprites};

use std::collections::HashSet;
use std::sync::{mpsc, Arc, RwLock};
use std::time::Duration;
use notify::{self, Watcher};

#[derive(Debug)]
pub enum WindowUpdateKind {
    Create,
    Delete,
    // ModSize, // buffer reallocation needed
    // Redraw, // size does not change, but needs redraw
    // Reschedule, // timing changed, need new
}

#[derive(Debug)]
pub struct WindowUpdate {
    pub spriteid: SpriteId,
    pub kind: WindowUpdateKind,
}

pub enum SensingUpdate {
    #[cfg(debug_assertions)]
    PluginDebugUpdage,
}

pub struct DirectoryWatcher {
    window_update_queue: mpsc::Sender<WindowUpdate>,
    sensing_update_queue: mpsc::Sender<SensingUpdate>,

    sprites: Arc<RwLock<Sprites>>,
    sensors: Arc<RwLock<Sensors>>,
    clipbank: Arc<RwLock<ClipBank>>,
}

impl DirectoryWatcher {
    pub fn new(
        window_update_queue: mpsc::Sender<WindowUpdate>,
        sensing_update_queue: mpsc::Sender<SensingUpdate>,
        sprites: Arc<RwLock<Sprites>>,
        sensors: Arc<RwLock<Sensors>>,
        clipbank: Arc<RwLock<ClipBank>>,
    ) -> Self {
        Self { window_update_queue, sensing_update_queue, sprites, sensors, clipbank }
    }

    // returns if should be restarted
    pub fn run(&mut self) -> bool {
        log::info!("DirectoryWatcher started running");

        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).unwrap();

        let sprite_path = &crate::base::path().sprite;
        if let Err(e) = watcher.watch(sprite_path, notify::RecursiveMode::Recursive) {
            log::error!("could not install notify watcher for '{sprite_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{sprite_path}'");
        }

        let plugin_path = &crate::base::path().plugin;
        if let Err(e) = watcher.watch(plugin_path, notify::RecursiveMode::Recursive) {
            log::info!("could not install notify watcher for '{plugin_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{plugin_path}'");
        }

        let running_path = &crate::base::path().running;
        if let Err(e) = watcher.watch(running_path, notify::RecursiveMode::NonRecursive) {
            log::info!("could not install notify watcher for '{running_path}' ({e})");
        } else {
            log::info!("installed notify watcher for '{running_path}'");
        }

        let mut dirty: HashSet<AppPath> = HashSet::new();
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
                    log::debug!("ignoring inotify event {event:?}");
                    None
                }
            }
        }

        // using per-watcher debounce; rather than per-file debounce
        let ret = 'outer: loop {
            // wait for next event to happen
            if let Ok(Ok(res)) = rx.recv() {
                if let Some(path) = filter_notify_event(res) {
                    log::debug!("got watcher event (wake) '{path}'");
                    dirty.insert(path);
                }
            }

            // keep collecting till there's no updates for a duration
            while let Ok(Ok(res)) = rx.recv_timeout(debounce_duration) {
                if let Some(path) = filter_notify_event(res) {
                    log::debug!("got watcher event (following) '{path}'");
                    dirty.insert(path);
                }
            }

            // process the updated files
            for path in dirty.drain() {
                log::debug!("processing updated file '{path}'");
                let keep_going = self.handle_file_reload(&path);
                if keep_going != 0 { break 'outer keep_going == 2; }
            }

            // exiting the function, drop the object to shut down other threads
        };

        log::info!("DirectoryWatcher exiting");
        ret
    }

    // 0: keep_going, 1: exit, 2: restart
    fn handle_file_reload(&mut self, path: &AppPath) -> u8 {
        match analize_path(path) {
            AppPathAnalisys::Clip(_)    => self.reload_webp(path),
            AppPathAnalisys::SpriteList => self.reload_list_toml(path),
            AppPathAnalisys::Sprite(_)  => self.reload_sprite_toml(path),

            AppPathAnalisys::Running    => {
                // halt if not exist
                return if crate::base::path().running.exists() {0} else {1};
            }

            #[cfg(debug_assertions)]
            AppPathAnalisys::PluginDebug => {
                self.sensing_update_queue.send(SensingUpdate::PluginDebugUpdage);
            }

            AppPathAnalisys::Plugin(_)  => {
                return 2;
            }

            AppPathAnalisys::TempL(_)   => unreachable!(),
            AppPathAnalisys::Log        => unreachable!(),
            AppPathAnalisys::Lock       => unreachable!(),
            AppPathAnalisys::Unknown    => {
                log::info!("ignoring updates from '{path}'");
            }
        }

        return 0;
    }

    pub fn reload_list_toml(&mut self, path: &AppPath) {
        log::info!("reloading list_toml at '{path}'");
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank= self.clipbank.write().expect("poisoned clipbank lock");
        sprites.reload(path, &mut sensors, &mut clipbank, |upd| {
            _ = self.window_update_queue.send(upd);
            // ignoring SendError, which only occurs when the consumer is dropped
        });
    }

    pub fn reload_sprite_toml(&mut self, path: &AppPath) {
        log::info!("reloading sprite_toml with '{path}'");
        // FIXME this may result in no updates, but we still acquires lock.
        // this may unnecessarily stagger the watcher if there is many un-tracked toml files are added to the directory
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank= self.clipbank.write().expect("poisoned clipbank lock");
        sprites.reload_sprite(path, &mut sensors, &mut clipbank, |upd| {
            _ = self.window_update_queue.send(upd);
            // ignoring SendError, which only occurs when the consumer is dropped
        });
    }

    pub fn reload_webp(&mut self, path: &AppPath) {
        // refresh clips in the clipbank
        let mut clipbank = self.clipbank.write().expect("poisoned clipbank lock");
        let sprites = self.sprites.read().expect("poisoned sprites lock");
        // FIXME this may result in no updates, but we still acquires lock.
        // this may unnecessarily stagger the watcher if there is many un-tracked image files are added to the directory
        for (clipid, err) in clipbank.reload(path) {
            if let Some(e) = err {
                log::error!("could not reload clip from '{path}' ({e:?})"); // FIXME more gracefull error report
                break;
            }

            // find sprites having the update clip as current clip, and notify window renderer
            for (spriteid, (decl, _)) in sprites.iter() {
                if let Some(scon) = decl.get_sprite() {
                    if scon.current_clip() == clipid {
                        // self.update_queue.send(WindowUpdate::ModBuffer(SpriteId(id)));
                        _ = self.window_update_queue.send(WindowUpdate { spriteid, kind: WindowUpdateKind::Delete });
                        _ = self.window_update_queue.send(WindowUpdate { spriteid, kind: WindowUpdateKind::Create });
                    }
                }
            }
        }
    }

}