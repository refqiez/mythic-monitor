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
        // Foundation::ERROR_SUCCESS,
        // Graphics::Dxgi::*,
        System::{
            Performance::*,
            SystemInformation::*,
        }
    }, core::*
    // Win32::Graphics::Dxgi::Common::*,
};
use std::collections::HashMap;
use std::mem;

/// Comprehensive PDH-based metrics collector
///
/// Uses a single batched query for efficiency.
/// Create once, update once per polling interval, read many times.
pub struct PdhMetrics {
    // Single query object for all PDH counters
    query: PDH_HQUERY,

    // CPU counters
    cpu_total_counter: PDH_HCOUNTER,
    cpu_per_core_counters: Vec<PDH_HCOUNTER>,

    // RAM counter
    // ram_counter: PDH_HCOUNTER,

    // Disk counters (per physical disk)
    disk_counters: HashMap<String, DiskCounters>,

    // Network counters (per interface)
    network_counters: HashMap<String, NetworkCounters>,

    // GPU counters (per GPU)
    // Note: PDH GPU counters are unreliable, we'll use DXGI as fallback
    // gpu_counters: Vec<GpuCounters>,

    // DXGI adapters for reliable VRAM readings
    // dxgi_adapters: Vec<IDXGIAdapter3>,

    // Cached values (for when PDH counters aren't available)
    core_count: usize,
}

#[derive(Clone)]
struct DiskCounters {
    read_bytes_counter: PDH_HCOUNTER,
    write_bytes_counter: PDH_HCOUNTER,
}

#[derive(Clone)]
struct NetworkCounters {
    bytes_sent_counter: PDH_HCOUNTER,
    bytes_received_counter: PDH_HCOUNTER,
}

#[derive(Clone)]
struct GpuCounters {
    utilization_counter: Option<PDH_HCOUNTER>,
    // VRAM via DXGI, not PDH
}

impl PdhMetrics {
    /// Create new metrics collector
    ///
    /// This does all the expensive setup:
    /// - Creates PDH query
    /// - Enumerates and adds all counters
    /// - Initializes DXGI for GPU
    ///
    /// Call once at startup.
    pub fn new() -> Self {
        unsafe {
            // Create the master query
            let mut query = PDH_HQUERY::default();
            let ret = PdhOpenQueryW(None, 0, &mut query);
            assert_eq!(ret, 0);

            // Add CPU counters
            let (cpu_total, cpu_per_core, core_count) = Self::add_cpu_counters(query);

            // Add RAM counter
            // let ram_counter = Self::add_ram_counter(query)?;

            // Add disk counters (enumerate all physical disks)
            let disk_counters = Self::add_disk_counters(query);

            // Add network counters (enumerate all interfaces)
            let network_counters = Self::add_network_counters(query);

            // Try to add GPU counters (may not be available)
            // let gpu_counters = Self::add_gpu_counters(query);

            // Initialize DXGI for reliable VRAM readings
            // let dxgi_adapters = Self::init_dxgi_adapters();

            // Perform initial collection (required for delta-based counters)
            let ret = PdhCollectQueryData(query);
            assert_eq!(ret, 0);

            Self {
                query,
                cpu_total_counter: cpu_total,
                cpu_per_core_counters: cpu_per_core,
                // ram_counter,
                disk_counters,
                network_counters,
                // gpu_counters,
                // dxgi_adapters,
                core_count,
            }
        }
    }

    /// Update all metrics
    ///
    /// Call this once per polling interval (e.g., every 1 second).
    /// This collects ALL counters in a single syscall.
    pub fn update(&mut self) {
        unsafe {
            let ret = PdhCollectQueryData(self.query);
            assert_eq!(ret, 0);
        }
    }

    // ========================================================================
    // CPU Metrics
    // ========================================================================

    /// Get overall CPU usage (0.0 - 100.0)
    pub fn cpu_usage_total(&self) -> f32 {
        Self::get_counter_value(self.cpu_total_counter)
    }

    /// Get per-core CPU usage
    /// Returns Vec of percentages (0.0 - 100.0) for each core
    pub fn cpu_usage_per_core(&self) -> Vec<f32> {
        self.cpu_per_core_counters
            .iter()
            .map(|&counter| Self::get_counter_value(counter))
            .collect()
    }

    /// Get number of CPU cores
    pub fn cpu_core_count(&self) -> usize {
        self.core_count
    }

    // ========================================================================
    // RAM Metrics
    // ========================================================================

    /// Get RAM usage percentage (0.0 - 100.0)
    pub fn ram_usage(&self) -> f32 {
        // Self::get_counter_value(self.ram_counter)

        unsafe {
            let mut mem_status: MEMORYSTATUSEX = mem::zeroed();
            mem_status.dwLength = mem::size_of::<MEMORYSTATUSEX>() as u32;

            if GlobalMemoryStatusEx(&mut mem_status).is_ok() {
                // let total = mem.ullTotalPhys;
                // let free = mem.ullAvailPhys;
                // let used = total - free; // this includes compressed memory
                mem_status.dwMemoryLoad as f32
            } else {
                log::error!("failed to get memory status");
                0.0
            }
        }
    }

    // ========================================================================
    // Disk Metrics
    // ========================================================================

    /// Get list of monitored disk names
    pub fn disk_names(&self) -> Vec<String> {
        self.disk_counters.keys().cloned().collect()
    }

    /// Get disk read bytes/sec for a specific disk
    /// Returns None if disk not found
    pub fn disk_read_bytes_per_sec(&self, disk_name: &str) -> Option<f32> {
        self.disk_counters.get(disk_name).map(|counters| {
            Self::get_counter_value(counters.read_bytes_counter)
        })
    }

    /// Get disk write bytes/sec for a specific disk
    /// Returns None if disk not found
    pub fn disk_write_bytes_per_sec(&self, disk_name: &str) -> Option<f32> {
        self.disk_counters.get(disk_name).map(|counters| {
            Self::get_counter_value(counters.write_bytes_counter)
        })
    }

    /// Get total disk read bytes/sec across all disks
    pub fn disk_read_bytes_per_sec_total(&self) -> f32 {
        self.disk_counters
            .values()
            .map(|c| Self::get_counter_value(c.read_bytes_counter))
            .sum()
    }

    /// Get total disk write bytes/sec across all disks
    pub fn disk_write_bytes_per_sec_total(&self) -> f32 {
        self.disk_counters
            .values()
            .map(|c| Self::get_counter_value(c.write_bytes_counter))
            .sum()
    }

    // ========================================================================
    // Network Metrics
    // ========================================================================

    /// Get list of monitored network interface names
    pub fn network_interface_names(&self) -> Vec<String> {
        self.network_counters.keys().cloned().collect()
    }

    /// Get network upload bytes/sec for specific interface
    /// Returns None if interface not found
    pub fn network_upload_bytes_per_sec(&self, interface: &str) -> Option<f32> {
        self.network_counters.get(interface).map(|counters| {
            Self::get_counter_value(counters.bytes_sent_counter)
        })
    }

    /// Get network download bytes/sec for specific interface
    /// Returns None if interface not found
    pub fn network_download_bytes_per_sec(&self, interface: &str) -> Option<f32> {
        self.network_counters.get(interface).map(|counters| {
            Self::get_counter_value(counters.bytes_received_counter)
        })
    }

    /// Get total network upload bytes/sec across all interfaces
    pub fn network_upload_bytes_per_sec_total(&self) -> f32 {
        self.network_counters
            .values()
            .map(|c| Self::get_counter_value(c.bytes_sent_counter))
            .sum()
    }

    /// Get total network download bytes/sec across all interfaces
    pub fn network_download_bytes_per_sec_total(&self) -> f32 {
        self.network_counters
            .values()
            .map(|c| Self::get_counter_value(c.bytes_received_counter))
            .sum()
    }

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
    // pub fn gpu_utilization(&self, gpu_index: usize) -> f32 {
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
    // pub fn gpu_vram_usage(&self, gpu_index: usize) -> f32 {
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
    //                 return (info.CurrentUsage as f64 / info.Budget as f64 * 100.0) as f32;
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

    /// Add CPU counters to query
    fn add_cpu_counters(query: PDH_HQUERY) -> (PDH_HCOUNTER, Vec<PDH_HCOUNTER>, usize) {
        unsafe {
            // Add total CPU counter
            let mut cpu_total = PDH_HCOUNTER::default();
            let ret = PdhAddCounterW(
                query,
                w!("\\Processor(_Total)\\% Processor Time"),
                0,
                &mut cpu_total,
            );
            assert_eq!(ret, 0);

            // Enumerate per-core counters
            let mut per_core = Vec::new();
            let num_cores = 12; // FIXME // logical cores

            for core_index in 0 .. num_cores {
                let path = format!("\\Processor({})\\% Processor Time\0", core_index);
                let path_wide: Vec<u16> = path.encode_utf16().collect();

                let mut counter = PDH_HCOUNTER::default();
                let ret = PdhAddCounterW(
                    query,
                    PCWSTR::from_raw(path_wide.as_ptr()),
                    0,
                    &mut counter,
                );
                assert_eq!(ret, 0);

                per_core.push(counter);
            }

            (cpu_total, per_core, num_cores)
        }
    }

    /// Add RAM counter to query
    ///
    /// this measures "committed memory" not physical ram allocation status.
    fn add_ram_counter(query: PDH_HQUERY) -> Result<PDH_HCOUNTER> {
        unsafe {
            let mut counter = PDH_HCOUNTER::default();
            let ret = PdhAddCounterW(
                query,
                w!("\\Memory\\% Committed Bytes In Use"),
                0,
                &mut counter,
            );
            assert_eq!(ret, 0);
            Ok(counter)
        }
    }

    /// Add disk counters to query
    /// Enumerates all physical disks
    fn add_disk_counters(query: PDH_HQUERY) -> HashMap<String, DiskCounters> {
        unsafe {
            let mut disk_counters = HashMap::new();

            let Some((_counters, instances)) = get_enum_object_items("LogicalDisk") else {
                return disk_counters;
            };

            // for counter in counters {
            //     println!("counter: {counter}");
            // }

            for disk_name in instances {
                // println!("instance: {disk_name}");

                let read_path = format!("\\LogicalDisk({})\\Disk Read Bytes/sec\0", disk_name);
                let write_path = format!("\\LogicalDisk({})\\Disk Write Bytes/sec\0", disk_name);

                let read_wide: Vec<u16> = read_path.encode_utf16().collect();
                let write_wide: Vec<u16> = write_path.encode_utf16().collect();

                let mut read_counter = PDH_HCOUNTER::default();
                let mut write_counter = PDH_HCOUNTER::default();

                let read_result = PdhAddCounterW(
                    query,
                    PCWSTR::from_raw(read_wide.as_ptr()),
                    0,
                    &mut read_counter,
                );

                let write_result = PdhAddCounterW(
                    query,
                    PCWSTR::from_raw(write_wide.as_ptr()),
                    0,
                    &mut write_counter,
                );

                if read_result == 0 && write_result == 0 {
                    disk_counters.insert(
                        disk_name.to_string(),
                        DiskCounters {
                            read_bytes_counter: read_counter,
                            write_bytes_counter: write_counter,
                        },
                    );
                } else {
                    log::error!("could not install counter for logical disk ({})", disk_name);
                }
            }

            disk_counters
        }
    }

    /// Add network counters to query
    /// Enumerates all network interfaces
    fn add_network_counters(query: PDH_HQUERY) -> HashMap<String, NetworkCounters> {
        unsafe {
            let mut network_counters = HashMap::new();

            let Some((_counters, instances)) = get_enum_object_items("Network Interface") else {
                return network_counters;
            };

            // for counter in counters {
            //     println!("counter: {counter}");
            // }

            for interface_name in instances {
                // println!("instance: {interface_name}");

                // Add counters for this interface
                let sent_path = format!("\\Network Interface({})\\Bytes Sent/sec\0", interface_name);
                let recv_path = format!("\\Network Interface({})\\Bytes Received/sec\0", interface_name);

                let sent_wide: Vec<u16> = sent_path.encode_utf16().collect();
                let recv_wide: Vec<u16> = recv_path.encode_utf16().collect();

                let mut sent_counter = PDH_HCOUNTER::default();
                let mut recv_counter = PDH_HCOUNTER::default();

                let sent_result = PdhAddCounterW(
                    query,
                    PCWSTR::from_raw(sent_wide.as_ptr()),
                    0,
                    &mut sent_counter,
                );

                let recv_result = PdhAddCounterW(
                    query,
                    PCWSTR::from_raw(recv_wide.as_ptr()),
                    0,
                    &mut recv_counter,
                );

                if sent_result == 0 && recv_result == 0 {
                    network_counters.insert(
                        interface_name.clone(),
                        NetworkCounters {
                            bytes_sent_counter: sent_counter,
                            bytes_received_counter: recv_counter,
                        },
                    );
                } else {
                    log::error!("could not install counter for network interface ({})", interface_name);
                }
            }

            network_counters
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

    //         let factory: Result<IDXGIFactory1> = CreateDXGIFactory1();
    //         if let Ok(factory) = factory {
    //             let mut adapter_index = 0;

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

    //         adapters
    //     }
    // }

    /// Helper to get a formatted counter value
    fn get_counter_value(counter: PDH_HCOUNTER) -> f32 {
        unsafe {
            let mut value = PDH_FMT_COUNTERVALUE::default();

            if 0 == PdhGetFormattedCounterValue(
                counter,
                PDH_FMT_DOUBLE,
                None,
                &mut value,
            ) {
                value.Anonymous.doubleValue as f32
            } else {
                0.0
            }
        }
    }
}

impl Drop for PdhMetrics {
    fn drop(&mut self) {
        unsafe {
            // Clean up PDH query
            let _ = PdhCloseQuery(self.query);
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

// ============================================================================
// Usage Example
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_metrics() {
        // Create metrics collector (one-time setup)
        let mut metrics = PdhMetrics::new();

        println!("=== System Information ===");
        println!("CPU cores: {}", metrics.cpu_core_count());
        // println!("GPUs detected: {}", metrics.gpu_count());
        println!("Network interfaces: {:?}", metrics.network_interface_names());
        println!("Disks: {:?}", metrics.disk_names());
        println!();

        // Wait a bit for initial samples
        thread::sleep(Duration::from_millis(500));

        // Collect metrics
        for i in 0..5 {
            println!("=== Sample {} ===", i + 1);

            // Update all metrics in one call
            metrics.update();

            // CPU
            println!("CPU Total: {:.1}%", metrics.cpu_usage_total());
            let per_core = metrics.cpu_usage_per_core();
            for (core, usage) in per_core.iter().enumerate() {
                println!("  Core {}: {:.1}%", core, usage);
            }

            // RAM
            println!("RAM: {:.1}%", metrics.ram_usage());

            // Disk
            println!("Disk Read (total): {:.2} MB/s",
                metrics.disk_read_bytes_per_sec_total() / 1_000_000.0);
            println!("Disk Write (total): {:.2} MB/s",
                metrics.disk_write_bytes_per_sec_total() / 1_000_000.0);

            // Network
            println!("Network Upload (total): {:.2} MB/s",
                metrics.network_upload_bytes_per_sec_total() / 1_000_000.0);
            println!("Network Download (total): {:.2} MB/s",
                metrics.network_download_bytes_per_sec_total() / 1_000_000.0);

            // // GPU
            // for gpu_idx in 0..metrics.gpu_count() {
            //     println!("GPU {} Utilization: {:.1}%", gpu_idx, metrics.gpu_utilization(gpu_idx));
            //     println!("GPU {} VRAM: {:.1}%", gpu_idx, metrics.gpu_vram_usage(gpu_idx));

            //     if let Some((used, total)) = metrics.gpu_vram_bytes(gpu_idx) {
            //         println!("  VRAM: {:.2} / {:.2} GB",
            //             used as f64 / 1_000_000_000.0,
            //             total as f64 / 1_000_000_000.0);
            //     }
            // }

            println!();
            thread::sleep(Duration::from_secs(1));
        }
    }
}

#[repr(u16)]
enum Sid {
    CpuUsageTotal = 1,
    CpuCoreCount,
    // Cpu_usage_per_core,

    // RamUsage,
    RamUsagePercent,

    DiskReadTotal,
    DiskWriteTotal,
    // Disk_read_per_dev,
    // Disk_write_per_dev,

    NetworkUploadTotal,
    NetworkDownloadTotal,
    // Network_upload_per_intf,
    // Network_download_per_intf,
}

pub struct BuiltinSensor {
    inner: PdhMetrics,
}

impl BuiltinSensor {
    pub fn create() -> Self {
        Self { inner: PdhMetrics::new(), }
    }

    pub fn refresh(&mut self) {
        self.inner.update()
    }

    pub fn read(&self, sid: u16) -> f32 {
        use Sid::*;
        if sid == 0 || NetworkDownloadTotal as u16 <= sid{
            log::error!("unrecognized sid {sid}");
            return 0.0
        }
        let sid: Sid = unsafe { std::mem::transmute(sid) };

        match sid {
            CpuUsageTotal => self.inner.cpu_usage_total() / 100.0,
            CpuCoreCount => self.inner.cpu_core_count() as f32,
            // Cpu_usage_per_core,

            // RamUsage => self.inner.ram_usage(),
            RamUsagePercent => self.inner.ram_usage(),

            DiskReadTotal => self.inner.disk_read_bytes_per_sec_total(),
            DiskWriteTotal => self.inner.disk_write_bytes_per_sec_total(),
            // Disk_read_per_dev,
            // Disk_write_per_dev,

            NetworkUploadTotal => self.inner.network_upload_bytes_per_sec_total(),
            NetworkDownloadTotal => self.inner.network_download_bytes_per_sec_total(),
            // Network_upload_per_intf,
            // Network_download_per_intf,
        }
    }

    // TODO manage reference counter
    pub fn register(&mut self, path: &str) -> u16 {
        use Sid::*;
        (match path {
            "cpu" => CpuUsageTotal,
            "cpu.num" => CpuCoreCount,

            "mem.p" => RamUsagePercent,

            "disk.read" => DiskReadTotal,
            "disk.write" => DiskWriteTotal,

            "net.up" => NetworkUploadTotal,
            "net.down" => NetworkDownloadTotal,

            _ => return 0,
        }) as u16
    }

    pub fn unregister(&mut self, sid: u16) {
    }
}