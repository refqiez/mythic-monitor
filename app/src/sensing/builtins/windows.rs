
mod pdh;

use pdh::{PdhMetrics, MultiCounterReuseBuffer, CounterKind, BytesForNameCounterKind};

use super::super::sensor::OpaqueError;

#[repr(u32)]
enum OpaqueErrorKind {
    // Registering errors
    DiskSubfield,
    NetSubfield,
    Unrecognized,
    Trailing,
    TooMany,
    UnknownSid,

    // install/uninstall counter
    Install(CounterKind),
    Uninstall(CounterKind),

    Update,

    // Refreshing error
    // GetCpuUsage,
    GetRamStat,
    // GetDiskRead,
    // GetDiskWrite,
    // GetNetworkUp,
    // GetNetworkDown,
    Get(CounterKind),
}

impl OpaqueErrorKind {
    fn generate(self, misc: u16) -> OpaqueError<'static> {
        use OpaqueErrorKind::*;
        let (errhigh, errlow, message) = match &self {
            DiskSubfield => (1, 0, "Subfields of 'disk' can be .read, .write or a single character disk label"),
            NetSubfield  => (2, 0, "Subfields of 'net' can be .up, .down or network name interface"),
            Unrecognized => (3, 0, "Unrecognized identifier path"),
            Trailing     => (4, 0, "Unrecognized trailing subfields"),
            TooMany      => (5, 0, "Too many unique identifier path registeration"),
            UnknownSid   => (6, 0, "Tried to unregister unseen sid"),

            Install(CounterKind::CpuUsage)                                  => (7, 1, "could not install cpu performance counter"),
            Install(CounterKind::BFN(BytesForNameCounterKind::DiskRead))    => (7, 2, "could not install disk read performance counter"),
            Install(CounterKind::BFN(BytesForNameCounterKind::DiskWrite))   => (7, 4, "could not install disk write performance counter"),
            Install(CounterKind::BFN(BytesForNameCounterKind::NetworkUp))   => (7, 8, "could not install network sent performance counter"),
            Install(CounterKind::BFN(BytesForNameCounterKind::NetworkDown)) => (7,16, "could not install network recv performance counter"),

            Uninstall(CounterKind::CpuUsage)                                  => (8, 1, "could not uninstall cpu performance counter"),
            Uninstall(CounterKind::BFN(BytesForNameCounterKind::DiskRead))    => (8, 2, "could not uninstall disk read performance counter"),
            Uninstall(CounterKind::BFN(BytesForNameCounterKind::DiskWrite))   => (8, 4, "could not uninstall disk write performance counter"),
            Uninstall(CounterKind::BFN(BytesForNameCounterKind::NetworkUp))   => (8, 8, "could not uninstall network sent performance counter"),
            Uninstall(CounterKind::BFN(BytesForNameCounterKind::NetworkDown)) => (8,16, "could not uninstall network recv performance counter"),

            Update => (9, 0, "could not properly refresh builtin PDH metrics"),

            GetRamStat                                                    => (10, 0, "could not fetch RAM status"),
            Get(CounterKind::CpuUsage)                                    => (10, 1, "could not fetch cpu usage using PHD"),
            Get(CounterKind::BFN(BytesForNameCounterKind::DiskRead))      => (10, 2, "could not fetch disk reads using PHD"),
            Get(CounterKind::BFN(BytesForNameCounterKind::DiskWrite))     => (10, 4, "could not fetch disk writes using PHD"),
            Get(CounterKind::BFN(BytesForNameCounterKind::NetworkUp))     => (10, 8, "could not fetch network uploads using PHD"),
            Get(CounterKind::BFN(BytesForNameCounterKind::NetworkDown))   => (10,16, "could not fetch network downloads using PHD"),
        };

        // note that we need unique errcode for each error kind since the message silence mechanism works on errcode.
        OpaqueError {
            errcode: errhigh << 16 | errlow,
            message: Ok(message),
            misc,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SenseKindRam { Usage, Total, Free }

// Definition order of variants matters.
// > When `derive`d on enums, variants are ordered by their top-to-bottom discriminant order.
// >    From std::cmp::PartialOrd doc.
// The order is used to sort IdMapItems, so it should arrange sense kinds that uses same counters together.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SenseKind {
    CpuCoreCount,
    CpuUsage(Option<u32>), // None for total

    Ram(SenseKindRam),

    DiskPresence(Vec<u16>),
    DiskRead(Option<Vec<u16>>), // None for total
    DiskWrite(Option<Vec<u16>>), // None for total

    NetworkPresence(Vec<u16>),
    NetworkUp(Option<Vec<u16>>), // None for total
    NetworkDown(Option<Vec<u16>>), // None for total
}

impl SenseKind {
    fn get_device(&self) -> Option<&[u16]> {
        use SenseKind::*;
        match self {
            CpuCoreCount | CpuUsage(_) | Ram(_) => None,
            DiskRead(dev) | DiskWrite(dev) | NetworkUp(dev) | NetworkDown(dev)
                => dev.as_ref().map(|v| v.as_slice()),
            DiskPresence(dev) | NetworkPresence(dev) => Some(dev.as_slice()),
        }
    }

    fn to_counter_kind(&self) -> Option<CounterKind> {
        use BytesForNameCounterKind::*;
        match self {
            SenseKind::CpuUsage(_)         => Some(CounterKind::CpuUsage),
            SenseKind::DiskPresence(_)     => Some(CounterKind::BFN(DiskRead)),
            SenseKind::DiskRead(_)         => Some(CounterKind::BFN(DiskRead)),
            SenseKind::DiskWrite(_)        => Some(CounterKind::BFN(DiskWrite)),
            SenseKind::NetworkPresence(_)  => Some(CounterKind::BFN(NetworkUp)),
            SenseKind::NetworkUp(_)        => Some(CounterKind::BFN(NetworkUp)),
            SenseKind::NetworkDown(_)      => Some(CounterKind::BFN(NetworkDown)),
            SenseKind::CpuCoreCount | SenseKind::Ram(_) => None,
        }
    }

    fn parse(path: &str) -> Result<(SenseKind, f64), OpaqueError<'static>> {
        fn split_number_prefix(s: &str) -> Option<(u32, &'_ str)> {
            let Some(s) = s.strip_prefix('.') else { return None };
            let prefix_len = s.char_indices().find(|(_,c)| !c.is_digit(10)).map(|(i,_)| i).unwrap_or(s.len());
            if prefix_len == 0 { return None; }
            let (prefix, rest) = (&s[..prefix_len], &s[prefix_len..]); // s[s.len()..] is safe, returns empty string
            prefix.parse::<u32>().ok().map(|num| (num, rest))
        }

        fn split_disklabel_prefix(s: &str) -> Option<(&'_ str, &'_ str)> {
            let Some(s) = s.strip_prefix('.') else { return None };
            let Some(c) = s.chars().next() else { return None };
            if ! c.is_ascii_alphabetic() { return None };
            let prefix_len = c.len_utf8();
            Some((&s[..prefix_len], &s[prefix_len..]))
        }

        fn split_interfacename_prefix(s: &str) -> Option<(&'_ str, &'_ str)> {
            let Some(s) = s.strip_prefix('.') else { return None };
            let prefix_len = s.char_indices().find(|(_,c)| *c != '.').map(|(i,_)| i).unwrap_or(s.len());
            if prefix_len == 0 { return None; }
            Some((&s[..prefix_len], &s[prefix_len..]))
        }

        fn split_ema_prefix(s: &str) -> Option<(f64, &'_ str)> {
            let Some(s) = s.strip_prefix(".ema.") else { return None };
            let num_len = s.char_indices().find(|(_,c)| !c.is_digit(10)).map(|(i,_)| i).unwrap_or(s.len());
            if num_len == 0 { return None; }
            let effective_num_len = std::cmp::min(10, num_len); // significance will be cut by EMA_EPSILON anyway
            let num = s[..effective_num_len].parse::<u32>().ok()?;
            let coef = num as f64 / 10u32.pow(effective_num_len as u32) as f64;
            Some((coef, &s[num_len..]))
        }

        use SenseKind::*;

        let (sense_kind, rest, rest_idx) =
            if let Some(path) = path.strip_prefix("cpu") {
                if let Some(path) = path.strip_prefix(".num") {
                    (CpuCoreCount, path, 3)
                } else if let Some((i, path)) = split_number_prefix(path) {
                    (CpuUsage(Some(i)), path, 3)
                } else {
                    (CpuUsage(None), path, 2)
                }

            } else if let Some(path) = path.strip_prefix("mem") {
                if let Some(path) = path.strip_prefix(".total") {
                    (Ram(SenseKindRam::Total), path, 3)
                } else if let Some(path) = path.strip_prefix(".avail") {
                    (Ram(SenseKindRam::Free), path, 3)
                } else {
                    (Ram(SenseKindRam::Usage), path, 2)
                }

            } else if let Some(path) = path.strip_prefix("disk") {
                if let Some(path) = path.strip_prefix(".read") {
                    (DiskRead(None), path, 3)
                } else if let Some(path) = path.strip_prefix(".write") {
                    (DiskWrite(None), path, 3)
                } else if let Some((label, path)) = split_disklabel_prefix(path) {
                    let label = label.encode_utf16().collect();
                    if let Some(path) = path.strip_prefix(".read") {
                        (DiskRead(Some(label)), path, 4)
                    } else if let Some(path) = path.strip_prefix(".write") {
                        (DiskWrite(Some(label)), path, 4)
                    } else {
                        (DiskPresence(label), path, 3)
                    }
                } else {
                    return Err(OpaqueErrorKind::DiskSubfield.generate(2));
                }

            } else if let Some(path) = path.strip_prefix("net") {
                if let Some(path) = path.strip_prefix(".up") {
                    (NetworkUp(None), path, 3)
                } else if let Some(path) = path.strip_prefix(".down") {
                    (NetworkDown(None), path, 3)
                } else if let Some((label, path)) = split_interfacename_prefix(path) {
                    let label = label.encode_utf16().collect();
                    if let Some(path) = path.strip_prefix(".up") {
                        (NetworkUp(Some(label)), path, 4)
                    } else if let Some(path) = path.strip_prefix(".down") {
                        (NetworkDown(Some(label)), path, 4)
                    } else {
                        (NetworkPresence(label), path, 3)
                    }
                } else {
                    return Err(OpaqueErrorKind::NetSubfield.generate(2));
                }

            } else {
                return Err(OpaqueErrorKind::Unrecognized.generate(0));
            };

        let (ema, rest, rest_idx) =
            if let Some((ema, rest)) = split_ema_prefix(rest) {
                (ema, rest, rest_idx + 2)
            } else {
                (1.0, rest, rest_idx)
            };

        if ! rest.is_empty() {
            return Err(OpaqueErrorKind::Trailing.generate(rest_idx));
        }

        Ok((sense_kind, ema))
    }
}

#[derive(Debug)]
struct IdMapItem {
    kind: SenseKind,
    ema: f64,       // ema coefficient
    idx: usize,     // idx in data
    rc: u32,        // ref count
}
const EMA_EPSILON: f64 = 0.0001;

// cannot implement Ord since it does not satisfies Eq's trasitivity rule.
impl IdMapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.kind.cmp(&other.kind) {
            std::cmp::Ordering::Equal => {
                debug_assert!(!self.ema.is_nan());
                debug_assert!(!other.ema.is_nan());
                let diff = self.ema - other.ema;
                if diff.abs() < EMA_EPSILON {
                    std::cmp::Ordering::Equal
                } else if diff < 0.0 {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            }
            ord => ord,
        }
    }
}

pub struct BuiltinSensor {
    inner: PdhMetrics,
    // sorted list of currently allocated sensing keys
    idmap: Vec<IdMapItem>,

    pub data: Vec<f64>,
}

impl BuiltinSensor {
    pub fn create() -> Self {
        let pdh = PdhMetrics::new();
        let mut data = vec![];
        let mut idmap = vec![];

        // core count is at 0 and will never be revoked
        data.push(pdh.get_cpu_count() as f64);
        idmap.push(IdMapItem { kind: SenseKind::CpuCoreCount, idx: 0, rc: 1, ema: 1.0 });

        Self { inner: pdh, idmap, data, }
    }

    // fn make_opaque_err(success: bool, kind: SenseReadErrorKind) -> Result<(), OpaqueError<'static>> {
    //     use SenseReadErrorKind::*;
    //     if ! success {
    //         let msg = match kind {
    //         };

    //         Err(OpaqueError { errcode: kind as u32, message: Ok(msg), misc: 0 })
    //     } else {
    //         Ok(())
    //     }
    // }

    pub fn refresh(&mut self) -> Result<(), OpaqueError<'static>> {
        let success = self.inner.update();
        if !success { return Err(OpaqueErrorKind::Update.generate(0)); }

        let mut buffer = MultiCounterReuseBuffer::new();
        let mut iter = self.idmap.iter().filter(|x| x.rc > 0).peekable();

        // TODO continue after error?
        while let Some(item) = iter.peek() {
            use SenseKind::*;
            match &item.kind {
                CpuCoreCount => {
                    let IdMapItem { idx, .. } = iter.next().unwrap();
                    assert_eq!(*idx, 0);
                    // cpu core count is conatnat, it should always points to 0'th index regardless of ema
                }

                CpuUsage(_) => {
                    // I tried to dynamic stack allocated array here (like in c) but rust prevents it.
                    let corecount = self.inner.get_cpu_count() as usize;
                    let mut cache = vec![None; corecount + 1]; // the last one is total
                    let success = self.inner.get_cpu_usage_per_core(&mut cache[..], &mut buffer);
                    if !success { return Err(OpaqueErrorKind::Get(CounterKind::CpuUsage).generate(0)); }
                    // use empty cache on failure
                    while let Some(IdMapItem { kind: CpuUsage(i), .. }) = iter.peek() {
                        let IdMapItem { ema, idx, .. } = iter.next().unwrap();
                        let i = i.map(|i| i as usize).unwrap_or(corecount);
                        self.data[*idx] = (1.0 - *ema) * self.data[*idx] + ema * cache[i].unwrap_or(0.0) / 100.0;
                    }
                }

                Ram(_) => {
                    let (success, caches) =
                        if let Some(caches) = self.inner.get_ram_stat() {
                            (true, caches)
                        } else {
                            (false, (0, 0, 0))
                        };
                    if !success { return Err(OpaqueErrorKind::GetRamStat.generate(0)); }

                    while let Some(IdMapItem { kind: Ram(kind), .. }) = iter.peek() {
                        let IdMapItem { ema, idx, .. } = iter.next().unwrap();
                        let cache = match kind {
                            SenseKindRam::Usage => caches.0 as f64 / 100.0,
                            SenseKindRam::Total => caches.1 as f64,
                            SenseKindRam::Free  => caches.2 as f64,
                        };
                        self.data[*idx] = (1.0 - *ema) * self.data[*idx] + ema * cache;
                    }
                }

                DiskRead(_) | DiskWrite(_) | DiskPresence(_) | NetworkUp(_) | NetworkDown(_) | NetworkPresence(_) => {
                    let access = {
                        let Some(CounterKind::BFN(kind)) = item.kind.to_counter_kind() else { unreachable!() };
                        let access = self.inner.get_disk_reads(&mut buffer, kind);

                        if let Some(access) = access {
                            access
                        } else {
                            return Err(OpaqueErrorKind::Get(CounterKind::BFN(kind)).generate(0));
                        }
                    };
                    let peek_counter_kind = item.kind.to_counter_kind();

                    // consider cache (buffer via access) can be used iff they are of same branch.
                    // this prevents buffer reuse between *Presence and *Read/Write, being slightly inefficient.
                    while iter.peek().map(|x| x.kind.to_counter_kind() == peek_counter_kind).unwrap_or(false) {
                        let IdMapItem { kind: k, ema, idx, .. } = iter.next().unwrap();
                        let cache = if let Some(dev) = k.get_device() {
                            access.get(dev)
                        } else {
                            let total = windows::core::w!("_Total"); // bind to prevent drop
                            let total = unsafe{ total.as_wide() };
                            access.get(total)
                        }.unwrap_or(0.0);
                        self.data[*idx] = (1.0 - *ema) * self.data[*idx] + ema * cache;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn register<'s>(&mut self, path: &'s str) -> Result<u16, OpaqueError<'static>> {

        let (kind, ema) = SenseKind::parse(path)?;
        let mut item = IdMapItem { kind, ema, idx: 0, rc: 0 };

        // overwrite ema of contant metric
        if item.kind == SenseKind::CpuCoreCount {
            item.ema = 1.0;
        }

        let is_bool = matches!(item.kind, SenseKind::DiskPresence(_) | SenseKind::NetworkPresence(_));

        let idmap_idx= match self.idmap.binary_search_by(|x| x.cmp(&item)) {
            Ok(idmap_idx) => {

                if self.idmap[idmap_idx].rc == 0 {
                    log::debug!("found hot idmap slot for registration of {path}: {:?}[{}]", self.idmap, idmap_idx);
                    // if recycling empty, the sensing value would have not updated for long
                    let data_idx = self.idmap[idmap_idx].idx;
                    self.data[data_idx] = 0.0;
                } else {
                    log::debug!("found cold idmap slot for registration of {path}: {:?}[{}]", self.idmap, idmap_idx);
                }
                self.idmap[idmap_idx].rc += 1;
                idmap_idx
            }

            Err(i) => {
                item.rc = 1;

                // search for empty slot backward
                let recycle = self.idmap[..i]
                    .iter().enumerate().rev()
                    .take_while(|(_, x)| x.kind.to_counter_kind() == item.kind.to_counter_kind())
                    .find(|(_, x)| x.rc == 0);

                // search for empty slot forwared
                let recycle = recycle.or_else(|| self.idmap[i..] // arr[arr.len()..] is safe
                    .iter().enumerate()
                    .take_while(|(_, x)| x.kind.to_counter_kind() == item.kind.to_counter_kind())
                    .find(|(_, x)| x.rc == 0)
                );

                // Note that elements in idmap and data have 1-to-1 relation, only that
                // elements of data cannot be relocated while idmap is frequent to change.
                // We know that # of empty entries in both container are same.
                // Furthermore, if an entry in data is empty iff corresponding entry in idmap is empty.

                if let Some((idmap_idx, _)) = recycle {
                    log::debug!("found empty idmap slot for registration of {path}: {:?}[{}]", self.idmap, idmap_idx);
                    // empty slot in idmap found, use corresponding data slot (also empty)
                    let data_idx = self.idmap[idmap_idx].idx;
                    self.data[data_idx] = 0.0;

                    self.idmap[idmap_idx].kind = item.kind;
                    self.idmap[idmap_idx].ema  = item.ema ;

                    idmap_idx

                } else {
                    // no empty slot found, create new one
                    log::debug!("inserting idmap slot for registration of {path}: {:?}", self.idmap);
                    if self.data.len() >= u16::MAX as usize {
                        return Err(OpaqueErrorKind::TooMany.generate(0));
                    }

                    let data_idx = self.data.len();
                    self.data.push(0.0);

                    item.idx = data_idx;
                    self.idmap.insert(i, item);

                    i
                }
            }
        };

        // In case there has been no registration before, we need to install the counter
        // We could traverse idmap to check if there alreay exists an entry for it,
        // since this operation is idempotent with no additional cost it is cheaper going without check.
        if let Some(counter_kind) = self.idmap[idmap_idx].kind.to_counter_kind() {
            let success = self.inner.install_counter(counter_kind);
            if !success {
                self.idmap[idmap_idx].rc -= 1;
                // if new slot has been inserted, it will be left empty and will be reused later.
                return Err(OpaqueErrorKind::Install(counter_kind).generate(0));
            }
        }

        // We could update data to prevent cold reading of just added sensing value here,
        // but update call out of regular refresh interval would ruin ema width.
        // self.inner.update();

        let mut sid = self.idmap[idmap_idx].idx;

        if is_bool {
            sid |= 0x8000;
        }

        Ok(sid as u16)
    }

    pub fn unregister(&mut self, sid: u16) -> Result<(), OpaqueError<'static>> {
        // If we don't properly register data & idmap buffers will ever grow leaking memory.

        // We don't clear empty slots in data & idmap buffer.
        // In 'register', we depend on empty entries of idmap to find empty slots to be reused.
        // If we want to clear them, then we need to manage empty entries of data by other means.
        // Since emtpy slot recycle only finds entries with compatible SenseKind, this may result in
        // many unused slots left in containers. (e.g. empty slots from disk.read will never be recycled for cpu.usage)
        // We decided to bare with this space inefficiency, to reduce computation.
        let idx = (sid & 0x7FFF) as usize;
        let (idmap_idx, item) = self.idmap.iter_mut().enumerate()
            .find(|(i, x)| x.rc > 0 && x.idx == idx)
            .ok_or(OpaqueErrorKind::UnknownSid.generate(0))?;
        item.rc -= 1;

        if item.rc == 0 {
            if let Some(counter_kind) = item.kind.to_counter_kind() {
                // let rc_sum = self.idmap.iter()
                //     .filter(|x| x.kind.to_counter_kind() == Some(counter_kind))
                //     .map(|x| x.rc)
                //     .sum::<u32>();

                // collect rc of counter_kind backward
                let rc_sum_front = self.idmap[..idmap_idx]
                    .iter().rev()
                    .take_while(|x| x.kind.to_counter_kind() == Some(counter_kind))
                    .map(|x| x.rc)
                    .sum::<u32>();

                // collect rc of counter_kind forwared
                let rc_sum_rear = self.idmap[idmap_idx..] // arr[arr.len()..] is safe
                    .iter()
                    .take_while(|x| x.kind.to_counter_kind() == Some(counter_kind))
                    .map(|x| x.rc)
                    .sum::<u32>();

                if rc_sum_front + rc_sum_rear == 0 {
                    self.inner.uninstall_counter(counter_kind);
                }
            }
        }
        Ok(())
    }

    // consume and drop self. pdh.drop will release pdh resources
    #[allow(unused)]
    pub fn destroy(self) {
    }
}