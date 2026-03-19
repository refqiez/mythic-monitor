use crate::worker::watcher::WindowUpdate;
use crate::sensing::Sensors;
use crate::sprites::{ClipBank, Frame, Sprites, SpriteDecl, SpriteId};

use std::sync::{mpsc, Arc, RwLock};

pub struct RenderDataInterface {
    pub update_queue: mpsc::Receiver<WindowUpdate>,

    sprites: Arc<RwLock<Sprites>>,
    sensors: Arc<RwLock<Sensors>>,
    clipbank: Arc<RwLock<ClipBank>>,
}

impl RenderDataInterface {
    pub fn new(
        update_queue: mpsc::Receiver<WindowUpdate>,
        sprites: Arc<RwLock<Sprites>>,
        sensors: Arc<RwLock<Sensors>>,
        clipbank: Arc<RwLock<ClipBank>>,
    ) -> Self {
        Self { update_queue, sprites, sensors, clipbank, }
    }

    pub fn with_frame<T>(&self, spriteid: SpriteId, f: impl FnOnce(&SpriteDecl, Frame) -> T) -> Option<T> {
        let clipbank = self.clipbank.read().expect("poisoned clipbank lock");
        let sprites = self.sprites.read().expect("poisoned sprites lock");
        let sprite = sprites.get(spriteid)?;
        let frame = sprite.get_sprite()?.get_frame(&clipbank);
        Some(f(sprite, frame))
    }

    pub fn with_sprite<T>(&self, spriteid: SpriteId, f: impl FnOnce(&SpriteDecl) -> T) -> Option<T> {
        let sprites = self.sprites.read().expect("poisoned sprites lock");
        let sprite = sprites.get(spriteid)?;
        Some(f(sprite))
    }

    pub fn advance(&self, spriteid: SpriteId) {
        let clipbank = self.clipbank.read().expect("poinsed clipbank lock");
        let sensors = self.sensors.read().expect("poisoned sensors lock");
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let sprite = sprites.get_mut(spriteid).unwrap(); // FIXME
        sprite.advance(&sensors, &clipbank);
    }
}