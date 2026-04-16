use crate::sprites::{ClipBank, ClipId};
use super::DecoderUpdate;

use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, RwLock, mpsc};
use std::collections::BinaryHeap;

pub struct ClipsDecoder {
    clipbank: Arc<RwLock<ClipBank>>,
    decode_queue: BinaryHeap<(usize, ClipId)>,

    update_queue: mpsc::Receiver<DecoderUpdate>,
}

impl ClipsDecoder {
    pub fn new(
        clipbank: Arc<RwLock<ClipBank>>,
        update_queue: mpsc::Receiver<DecoderUpdate>,
    ) -> Self {
        Self { clipbank, update_queue, decode_queue: BinaryHeap::new() }
    }

    // These two methods had to be static methods to decouple the borrow lifetime whiling reusing clipbank lock
    // I cannot give &mut self to these functions since RwLockGuard<ClipBank> holds immutable borrow of self.

    fn push_decode_task(clipid: ClipId, decode_queue: &mut BinaryHeap<(usize, ClipId)>, clipbank: &ClipBank) {
        let shortfall = clipbank.get_decode_shortfall(clipid);
        if shortfall == 0 { return; }
        decode_queue.push((shortfall, clipid));
    }

    fn regen_decode_queue(decode_queue: &mut BinaryHeap<(usize, ClipId)>, clipbank: &ClipBank) {
        decode_queue.clear();
        for (clipid, shortfall) in clipbank.list_shortfalls() {
            if shortfall == 0 { continue; }
            decode_queue.push((shortfall, clipid));
        }
    }

    fn process_update(upd: DecoderUpdate, decode_queue: &mut BinaryHeap<(usize, ClipId)>, clipbank: &ClipBank) {
        match upd {
            DecoderUpdate::Advanced(clipid) =>
                Self::push_decode_task(clipid, decode_queue, clipbank),
            DecoderUpdate::Rescan =>
                Self::regen_decode_queue(decode_queue, clipbank),
        }
    }

    // return if still connected; if not, we should shutdown
    fn consume_all_updates(decode_queue: &mut BinaryHeap<(usize, ClipId)>, update_queue: &mut mpsc::Receiver<DecoderUpdate>, clipbank: &ClipBank) -> bool {
        loop {
            match update_queue.try_recv() {
                Ok(upd) => Self::process_update(upd, decode_queue, clipbank),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    pub fn run(&mut self) {
        // TODO we may do load spreading to avoid cpu usage peak?
        loop {
            while let Some((shortfall, clipid)) = self.decode_queue.pop() {
                if self.clipbank.read().expect("poisoned clipbank lock").get_decode_shortfall(clipid) < shortfall { continue; } // stale signal

                let mut clipbank = self.clipbank.write().expect("poisoned clipbank lock");
                clipbank.decode_next_frame(clipid);

                if ! Self::consume_all_updates(&mut self.decode_queue, &mut self.update_queue, &clipbank) { return; }
            }

            // update_queue is empty, decode_queue is empty, we wait for new signal

            let Ok(upd) = self.update_queue.recv() else { return };
            let clipbank = self.clipbank.read().expect("poisoned clipbank lock");
            Self::process_update(upd, &mut self.decode_queue, &clipbank);

            if ! Self::consume_all_updates(&mut self.decode_queue, &mut self.update_queue, &clipbank) { return; }
        }
    }
}

/*
from animator
    - advance: decrease reserve
from watcher
    - add: decrease reserve
from decoder
    - decode: increase reserve
*/
