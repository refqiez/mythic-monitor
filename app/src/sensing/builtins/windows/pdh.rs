// Some tips found writing this.

// Windows provides 'PDH' api which enables access to performance measures.
// It is old yet reliable and the interface is not so polished (string query like SQL)
// User queries for a path. Path consists of 'object name', 'instance' and 'counter'.
// Powershell command `Get-Counter -ListSet *` will list all the counter paths that you can query with pdh
// (CounterSetName corresponds to object name)

// Available instances for a object name can be queried via calls to get_enum_object_items.
// It returns two iterators for 'counters' and 'instances'
// Path is generally "\object name(instance_name)\counter_name".
// Powershell command `Get-Counter [path]` prints the value for the query.
// Powershell `Get-Counter` supports wildcards in place of instance name, but not always.
// (you can use "\GPU Engine(*)\Utilization Percentage" to get overall gpu utilization.
// Wildcard support is not in rust API.
// Followings are objects that I found but did not use.

// GPU Engine
// Available counters are 'Utilization Percentage' and 'Running Time'
// There in an instance for each combination of
//   pid_<process>
//   luid_<adapter>
//   phys_<gpu index>
//   eng_<engine index>
//   engtype_<engine type>
// About 300+ instances at the moment of writing.
// This is because GPU Engine utilization is normalized per engine queue, not per GPU.
// ChatGPT suggests to calculate MAX(pid_<process> + phys_0 + eng_<n> + engtype_3D)
// and use it as a degree of GPU utilization.
// That will require hundreds of queries for all the instances and
// the query should be updated whenever 1. a process starts/stops or
// 2. a graphics context is created/destroyed.
// .. and that way more work than what I expected.

// GPU Local Adapter Memory, GPU Non Local Adapter Memory
// Available conter is 'Local Usage'.
// There are instances for GPU and GPU part. I couldn't quite understand why
// but GPU reveal sub-partition of it's memory, which is indicated with _part_{n} suffix
// in the instance name.

// GPU Process Memory
// Not sure what it represents but there are instances for (pid, luid, gpu) combinations.

// Thermal Zone Information
// According to ChatGPT, this supposed to represent temperature of CPU or motherboard in KELVIN.
// https://stackoverflow.com/questions/21815510/whats-the-temperature-returned-by-performancecounter-thermal-zone-information
// There are no available instances.
// For some reason, it gives 0xC0000BC4 (PDH_INVALID_PATH) when used without instance
// and 0x800007D5 (PDH_NO_DATA) when given random instance name.

// According to the documentation, all the win32 functions should return HRESULT or WIN32_ERROR type,
// but they return raw u32 value instead.
// Wrapping them in `Error::new(HRESULT::from_win32(ret), $descr)` compiles fine but Rust analizer
// raises false-error.
// Those error code values can be looked up at https://learn.microsoft.com/en-us/windows/win32/perfctrs/pdh-error-codes

// Rust ctate 'sysinfo' provides similar functionality.
// Being versatile yet heavier, inefficient, and insufficient.
// Reading the blog post on the implementation tells that its inner working is not much different from
// what I already wrote, so, leave it be.

// Powershell also seems to be able to use partial-wildcard in instance name as in
// 'Get-Counter "\GPU Engine(*engtype_3D)\Utilization Percentage"'
// https://superuser.com/a/1632853

use windows::{
    Win32::{
        Foundation::ERROR_SUCCESS,
        // Graphics::Dxgi::*,
        System::{
            Performance::*,
            SystemInformation::*,
        }
    },
    core::*
    // Win32::Graphics::Dxgi::Common::*,
};
use std::mem;

use crate::base::parse_simple_u8w;

/// Comprehensive PDH-based metrics collector
///
/// Uses a single batched query for efficiency.
/// Create once, update once per polling interval, read many times.
pub struct PdhMetrics {
    // Single query object for all PDH counters
    query: PDH_HQUERY,

    core_count: u32, // cached value

    cpu_counter: Option<PDH_HCOUNTER>,

    // RAM counter
    // ram_counter: PDH_HCOUNTER,

    disk_read_counter: Option<PDH_HCOUNTER>,
    disk_write_counter: Option<PDH_HCOUNTER>,

    network_sent_counter: Option<PDH_HCOUNTER>,
    network_recv_counter: Option<PDH_HCOUNTER>,

    // GPU counters (per GPU)
    // Note: PDH GPU counters are unreliable, we'll use DXGI as fallback
    // gpu_counters: Vec<GpuCounters>,

    // DXGI adapters for reliable VRAM readings
    // dxgi_adapters: Vec<IDXGIAdapter3>,
}

pub type MultiCounterReuseBuffer = Vec<PDH_FMT_COUNTERVALUE_ITEM_W>;

pub struct PdhArrayAccess<'a> {
    items: &'a [PDH_FMT_COUNTERVALUE_ITEM_W],
}

impl<'a> PdhArrayAccess<'a> {
    fn new(items: &'a [PDH_FMT_COUNTERVALUE_ITEM_W]) -> Self { Self { items } }
    pub fn nil() -> Self { Self { items: &[] } }

    pub fn get(&self, name: &[u16]) -> Option<f64> { unsafe {
        self.items.iter().find(|val| val.szName.as_wide() == name).and_then(|val|
            if val.FmtValue.CStatus != 0 { None }
            else { Some(val.FmtValue.Anonymous.doubleValue) }
        )
    }}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterKind {
    CpuUsage,
    BFN(BytesForNameCounterKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesForNameCounterKind {
    DiskRead,
    DiskWrite,
    NetworkUp,
    NetworkDown,
}

#[derive(Clone)]
struct GpuCounters {
    utilization_counter: Option<PDH_HCOUNTER>,
    // VRAM via DXGI, not PDH
}

macro_rules! install_counter_branch {
    ($self:expr, $name:ident, $query:literal) => {{
        if $self.$name.is_some() { return true; }
        let $name = Self::add_multicounter($self.query, w!($query));
        if $name.is_none() { return false; }
        $self.$name = $name;
    }};
}

macro_rules! uninstall_counter_branch {
    ($self:expr, $name:ident) => {{
        let Some($name) = $self.$name else { return true };
        let ret = PdhRemoveCounter($name);
        if ret != 0 { return false; }
        $self.$name = None;
    }};
}

impl PdhMetrics {
    pub fn new() -> Self {
        unsafe {
            // Create the master query
            let mut query = PDH_HQUERY::default();
            let ret = PdhOpenQueryW(None, 0, &mut query);
            assert_eq!(ret, 0);

            // We install every counter from the start.
            // This is against 'read-only-needed-with-refrence-count' philosophy of the Sensing lib.
            // TODO lazy install of counters
            // use PdhRemoveCounter

            let core_count = Self::get_core_counts().unwrap_or_else(|| {
                log::error!("could not get cpu core count, using 16 as default");
                16
            });

            // Try to add GPU counters (may not be available)
            // let gpu_counters = Self::add_gpu_counters(query);

            // Initialize DXGI for reliable VRAM readings
            // let dxgi_adapters = Self::init_dxgi_adapters();

            // Perform initial collection (required for delta-based counters)
            let ret = PdhCollectQueryData(query);
            // assert_eq!(ret, 0);
            // ignoring the query error; later update errors (during refresh) will be reported.

            Self {
                query,

                core_count,
                cpu_counter: None,
                disk_read_counter: None,
                disk_write_counter: None,
                network_sent_counter: None,
                network_recv_counter: None,
                // gpu_counters,
                // dxgi_adapters,
            }
        }
    }

    /// Update all metrics
    ///
    /// Call this once per polling interval (e.g., every 1 second).
    /// This collects ALL counters in a single syscall.
    /// Returns if update was successful.
    pub fn update(&mut self) -> bool {
        unsafe {
            let ret = PdhCollectQueryData(self.query);
            ret == 0
        }
    }

    pub fn get_cpu_count(&self) -> u32 {
        self.core_count
    }

    /// Get per-core CPU usage and overall usage (the last elem). (value is 0 - 100)
    /// Expects slice.len == cpu_count + 1.
    /// Returns false iff the read was successful, leave 'out' unchanged.
    /// Does nothing when cpu counter is not installed and returns true.
    pub fn get_cpu_usage_per_core(&self, out: &mut [Option<f64>], buffer: &mut MultiCounterReuseBuffer) -> bool { unsafe {
        let Some(counter) = self.cpu_counter else { return true };
        let Some(items) = Self::get_multicounter_value(counter, buffer) else { return false };

        for val in items {
            // parse_simple_u8w fails -> "_Total"
            let i = parse_simple_u8w(val.szName.as_wide()).map(|i| i as usize).unwrap_or(out.len()-1);
            if val.FmtValue.CStatus != 0 { continue; }
            out[i as usize] = Some(val.FmtValue.Anonymous.doubleValue);
        }

        true
    }}

    /// Get RAM usage percentage (0.0 - 100.0), total memory in bytes, free memory in bytes
    pub fn get_ram_stat(&self) -> Option<(u32, u64, u64)> {
        unsafe {
            let mut mem_status: MEMORYSTATUSEX = mem::zeroed();
            mem_status.dwLength = mem::size_of::<MEMORYSTATUSEX>() as u32;

            if GlobalMemoryStatusEx(&mut mem_status).is_ok() {
                Some((mem_status.dwMemoryLoad, mem_status.ullTotalPhys, mem_status.ullAvailPhys))
            } else { None }
        }
    }

    /// Get disk read/write or network sent/recv (in bytes/sec) of all devices and put them in the buffer.
    /// Call PdhArrayAccess.get to get actual values.
    /// Returns None if reading fails.
    pub fn get_disk_reads<'a>(&self, buffer: &'a mut MultiCounterReuseBuffer, kind: BytesForNameCounterKind) -> Option<PdhArrayAccess<'a>> { unsafe {
        use BytesForNameCounterKind::*;
        let counter = match kind {
            DiskRead    => self.disk_read_counter,
            DiskWrite   => self.disk_write_counter,
            NetworkUp   => self.network_sent_counter,
            NetworkDown => self.network_recv_counter,
        };
        let Some(counter) = counter else { return Some(PdhArrayAccess::nil()) };
        Self::get_multicounter_value(counter, buffer).map(PdhArrayAccess::new)
    }}

    pub fn install_counter(&mut self, kind: CounterKind) -> bool {
        match kind {
            CounterKind::CpuUsage =>                                    install_counter_branch!(self, cpu_counter, "\\Processor(*)\\% Processor Time"),
            CounterKind::BFN(BytesForNameCounterKind::DiskRead) =>      install_counter_branch!(self, disk_read_counter, "\\LogicalDisk(*)\\Disk Read Bytes/sec"),
            CounterKind::BFN(BytesForNameCounterKind::DiskWrite) =>     install_counter_branch!(self, disk_write_counter, "\\LogicalDisk(*)\\Disk Write Bytes/sec"),
            CounterKind::BFN(BytesForNameCounterKind::NetworkUp) =>     install_counter_branch!(self, network_sent_counter, "\\Network Interface(*)\\Bytes Sent/sec"),
            CounterKind::BFN(BytesForNameCounterKind::NetworkDown) =>   install_counter_branch!(self, network_recv_counter, "\\Network Interface(*)\\Bytes Received/sec"),
        }
        true
    }

    pub fn uninstall_counter(&mut self, kind: CounterKind) -> bool { unsafe {
        match kind {
            CounterKind::CpuUsage =>                                    uninstall_counter_branch!(self, cpu_counter),
            CounterKind::BFN(BytesForNameCounterKind::DiskRead) =>      uninstall_counter_branch!(self, disk_read_counter),
            CounterKind::BFN(BytesForNameCounterKind::DiskWrite) =>     uninstall_counter_branch!(self, disk_write_counter),
            CounterKind::BFN(BytesForNameCounterKind::NetworkUp) =>     uninstall_counter_branch!(self, network_sent_counter),
            CounterKind::BFN(BytesForNameCounterKind::NetworkDown) =>   uninstall_counter_branch!(self, network_recv_counter),
        }
        true
    }}

    // ========================================================================
    // GPU Metrics
    // ========================================================================

    // /// Get number of GPUs detected
    // pub fn gpu_count(&self) -> usize {
    //     self.dxgi_adapters.len()
    // }

    // /// Get GPU utilization percentage for specific GPU (0.0 - 100.0)
    // ///
    // /// Note: PDH GPU counters are often unavailable or unreliable.
    // /// Returns 0.0 if counter not available.
    // /// GPU index corresponds to DXGI adapter index.
    // pub fn gpu_utilization(&self, gpu_index: usize) -> f64 {
    //     if gpu_index >= self.gpu_counters.len() {
    //         return 0.0;
    //     }

    //     if let Some(counter) = self.gpu_counters[gpu_index].utilization_counter {
    //         Self::get_counter_value(counter)
    //     } else {
    //         // GPU utilization counter not available via PDH
    //         // Would need vendor-specific APIs (NVML, AMD ADL, etc.)
    //         0.0
    //     }
    // }

    // /// Get VRAM usage percentage for specific GPU (0.0 - 100.0)
    // ///
    // /// Uses DXGI for reliable readings.
    // /// Returns 0.0 if GPU index invalid or query fails.
    // pub fn gpu_vram_usage(&self, gpu_index: usize) -> f64 {
    //     if gpu_index >= self.dxgi_adapters.len() {
    //         return 0.0;
    //     }

    //     unsafe {
    //         let adapter = &self.dxgi_adapters[gpu_index];
    //         let mut info: DXGI_QUERY_VIDEO_MEMORY_INFO = mem::zeroed();

    //         if adapter.QueryVideoMemoryInfo(
    //             0, // Node 0
    //             DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    //             &mut info,
    //         ).is_ok() {
    //             if info.Budget > 0 {
    //                 return (info.CurrentUsage as f64 / info.Budget as f64 * 100.0) as f64;
    //             }
    //         }

    //         0.0
    //     }
    // }

    // /// Get VRAM usage in bytes for specific GPU
    // /// Returns (used, total) or None if query fails
    // pub fn gpu_vram_bytes(&self, gpu_index: usize) -> Option<(u64, u64)> {
    //     if gpu_index >= self.dxgi_adapters.len() {
    //         return None;
    //     }

    //     unsafe {
    //         let adapter = &self.dxgi_adapters[gpu_index];
    //         let mut info: DXGI_QUERY_VIDEO_MEMORY_INFO = mem::zeroed();

    //         if adapter.QueryVideoMemoryInfo(
    //             0,
    //             DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    //             &mut info,
    //         ).is_ok() {
    //             Some((info.CurrentUsage, info.Budget))
    //         } else {
    //             None
    //         }
    //     }
    // }

    // ========================================================================
    // Internal Helper Functions
    // ========================================================================

    fn get_core_counts() -> Option<u32> { unsafe {
        let querypath = w!("\\Processor(*)\\% Processor Time");

        // querying buffer len for wildcard expantion
        let mut buffer_len = 0;
        // should NEVER give this as argument directly.
        // it drops the array for the string right away, making HEAP_CORRUPTION error.
        let ret = PdhExpandWildCardPathW(
            None,
            querypath,
            None,
            &mut buffer_len,
            0
        );
        if ret != PDH_MORE_DATA { return None; }

        // getting paths after wildcard expantion
        let mut buffer = vec![0u16; buffer_len as usize];
        let ret = PdhExpandWildCardPathW(
            None,
            querypath,
            Some(PWSTR::from_raw(buffer.as_mut_ptr())),
            &mut buffer_len,
            0
        );
        if ret != 0 { return None; }

        let mut num_cores = 0;
        let mut start = 0;
        // let mut strings = vec![];
        loop {
            let (len, _) = buffer[start..].iter().enumerate().find(|(i,x)| **x == 0).unwrap();
            if len == 0 { break; }

            let s = String::from_utf16_lossy(&buffer[start..start+len]);
            // strings.push(s);
            num_cores += 1;
            start = start + len + 1;
        }
        num_cores -= 1; // account for _ToTal

        Some(num_cores)
    }}

    /// Add RAM counter to query
    ///
    /// this measures "committed memory" not physical ram allocation status.
    /// currently not in use
    fn add_ram_counter(query: PDH_HQUERY) -> PDH_HCOUNTER {
        unsafe {
            let mut counter = PDH_HCOUNTER::default();
            let ret = PdhAddCounterW(
                query,
                w!("\\Memory\\% Committed Bytes In Use"),
                0,
                &mut counter,
            );
            assert_eq!(ret, 0);
            counter
        }
    }

    /// Try to add GPU counters to query
    /// These are often not available, so we return a Vec that may be empty or have None values
    fn add_gpu_counters(query: PDH_HQUERY) -> Vec<GpuCounters> {
        unsafe {
            let mut gpu_counters = Vec::new();

            // Try to enumerate GPU engines
            // Note: This requires vendor performance counter providers to be installed
            // Often not available, so we handle errors gracefully

            // wildcard is not supported in this api. this counter will always return 0;
            // we need to manually collect engine queue utilization for 50+ instances
            // i'm not sure if I want it. leaving it for later.
            // // This is a wildcard that should match all GPU engines
            let mut counter = PDH_HCOUNTER::default();
            let ret = PdhAddCounterW(
                query,
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                0,
                &mut counter,
            );

            if ret == 0 {
                // Successfully added GPU counter
                gpu_counters.push(GpuCounters {
                    utilization_counter: Some(counter),
                });
            } else {
                // GPU counters not available
                // This is normal on many systems
                gpu_counters.push(GpuCounters {
                    utilization_counter: None,
                });
            }

            gpu_counters
        }
    }

    // /// Initialize DXGI adapters for GPU VRAM monitoring
    // fn init_dxgi_adapters() -> Vec<IDXGIAdapter3> {
    //     unsafe {
    //         let mut adapters = Vec::new();
    //
    //         let factory: Result<IDXGIFactory1> = CreateDXGIFactory1();
    //         if let Ok(factory) = factory {
    //             let mut adapter_index = 0;
    //
    //             loop {
    //                 match factory.EnumAdapters1(adapter_index) {
    //                     Ok(adapter) => {
    //                         // Try to get IDXGIAdapter3 interface (needed for QueryVideoMemoryInfo)
    //                         if let Ok(adapter3) = adapter.cast::<IDXGIAdapter3>() {
    //                             adapters.push(adapter3);
    //                         }
    //                         adapter_index += 1;
    //                     }
    //                     Err(_) => break,
    //                 }
    //             }
    //         }
    //
    //         adapters
    //     }
    // }

    fn add_multicounter(query: PDH_HQUERY, querypath: PCWSTR) -> Option<PDH_HCOUNTER> { unsafe {
        let mut counter= PDH_HCOUNTER::default();
        let ret = PdhAddCounterW(
            query,
            querypath,
            0,
            &mut counter,
        );
        if ret == 0 { Some(counter) } else { None }
    }}

    /// Helper to get a formatted counter value
    fn get_counter_value(counter: PDH_HCOUNTER) -> Option<f64> {
        unsafe {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            if 0 == PdhGetFormattedCounterValue(
                counter,
                PDH_FMT_DOUBLE,
                None,
                &mut value,
            ) {
                if value.CStatus == 0 {Some(value.Anonymous.doubleValue)} else {None}
            } else {
                None
            }
        }
    }

    /// This function is unsafe since accessing buffer beyond buffer_count will give jibberish, and it is not guarded.
    /// Resizing buffer without clearing may result in HEAP_CORRUPT panic.
    unsafe fn get_multicounter_value<'a>(counter: PDH_HCOUNTER, buffer: &'a mut MultiCounterReuseBuffer) -> Option<&'a [PDH_FMT_COUNTERVALUE_ITEM_W]> {
        // For cpu_counter, since we already know the number of Items that will be returned, I tried to use the following instead of making double call.
        // > let mut buffer_count = self.cpu_core_count() + 1;
        // > let mut buffer_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() as u32 * buffer_count;
        // But in fact, buffer_size != buffer_count * item_size; windows requests us larger buffer size than that.
        // PdhGetFormattedCounterArray does not allocate space for item.szName itself, it just uses the following space in the buffer
        // after all the countervalue items to store PCWSTR contents.
        let mut buffer_size = 0;
        let mut buffer_count = 0;
        let ret = PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buffer_size,
            &mut buffer_count,
            None,
        );
        if ret != PDH_MORE_DATA { return None; }

        // We could just do vec![0u8; buffer_size]; Vec::from_raw_parts(buffer.as_ptr) but that may break the memory alignment,
        // which is expected by from_raw_parts. so we just bear with slight inefficient allocation
        let itemsize = mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let buflen = ((buffer_size as usize + itemsize - 1) / itemsize) * itemsize;
        buffer.resize(buflen, PDH_FMT_COUNTERVALUE_ITEM_W::default());
        let ret = PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buffer_size,
            &mut buffer_count,
            Some(buffer.as_mut_ptr()),
        );
        if ret != 0 { return None; }

        // You can actually see the stored PWSTR values in the buffer
        // > let start = itemsize * buffer_count as usize;
        // > let slice_len = len - start;
        // > let ptr = (buffer.as_ptr() as *const u8).add(start);
        // > let slice = std::slice::from_raw_parts(ptr as *const u16, slice_len / 2);
        // > println!("{slice:?}");
        Some(&buffer[..buffer_count as usize])
    }
}

impl Drop for PdhMetrics {
    fn drop(&mut self) {
        unsafe {
            _ = PdhCloseQuery(self.query);
            // counters are owned by query, we don't need to clear them manually
        }
    }
}

#[derive(Debug)]
struct EnumObjectItems {
    i: usize,
    buf: Vec<u16>,
}

fn get_enum_object_items(object_name: &str ) -> Option<(EnumObjectItems, EnumObjectItems)> {
    unsafe {

        // Expand wildcard to get all instances
        let mut counter_len = 0u32;
        let mut instance_len = 0u32;

        // First call to get buffer size
        let ret = PdhEnumObjectItemsW(
            None,
            None,
            PWSTR::from_raw(
                object_name.encode_utf16().chain(Some(0)).collect::<Vec<_>>().as_mut_ptr(),
            ),
            None,
            &mut counter_len,
            None,
            &mut instance_len,
            PERF_DETAIL_WIZARD,
            0,
        );
        // println!("{counter_len} {instance_len}");
        assert_eq!(ret, PDH_MORE_DATA);

        if instance_len == 0 {
            // No instances found, return empty
            return None;
        }

        // Allocate buffer for instance names
        let mut counters: Vec<u16> = vec![0; counter_len as usize];
        let mut instances: Vec<u16> = vec![0; instance_len as usize];

        let ret = PdhEnumObjectItemsW(
            None,
            None,
            PWSTR::from_raw( // you MUST recreate this object; reusing previous one will cause 0xC0000BB8 (PDH_CSTATUS_NO_OBJECT)
                object_name.encode_utf16().chain(Some(0)).collect::<Vec<_>>().as_mut_ptr(),
            ),
            Some(PWSTR(counters.as_mut_ptr())),
            &mut counter_len,
            Some(PWSTR(instances.as_mut_ptr())),
            &mut instance_len,
            PERF_DETAIL_WIZARD,
            0,
        );
        assert_eq!(ret, 0);

        return Some((
            EnumObjectItems {
                i: 0,
                buf: counters,
            },
            EnumObjectItems {
                i: 0,
                buf: instances,
            },
        ));
    }
}

impl Iterator for EnumObjectItems {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {

        // Parse names (null-terminated strings in buffer)
        if self.i >= self.buf.len() || self.buf[self.i] == 0 {
            return None;
        }

        // Find end of this string
        let start = self.i;
        while self.i < self.buf.len() && self.buf[self.i] != 0 {
            self.i += 1;
        }

        let ret = if self.i == start {
            None
        } else {
            Some(String::from_utf16_lossy(&self.buf[start..self.i]))
        };

        self.i += 1; // skip null chark

        return ret;
    }
}

