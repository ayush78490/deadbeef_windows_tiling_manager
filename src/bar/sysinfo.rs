//! System statistics for the bar, each from the native API:
//!
//! | stat        | source |
//! |-------------|--------|
//! | CPU         | `GetSystemTimes` deltas |
//! | memory      | `GlobalMemoryStatusEx` |
//! | temperature | PDH `\Thermal Zone Information(*)\Temperature` (ACPI zones, Kelvin) |
//! | battery     | `GetSystemPowerStatus` |
//! | Wi-Fi       | Native WLAN API (`WlanQueryInterface`, current connection) |
//! | Bluetooth   | `BluetoothFindFirstRadio` / `BluetoothFindFirstDevice` |
//! | volume      | Core Audio `IAudioEndpointVolume` on the default render device |
//!
//! Anything a machine doesn't have (no battery, no thermal zones, Windows
//! withholding the SSID without location permission) degrades to `None`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::core::{w, GUID};
use windows::Win32::Devices::Bluetooth::*;
use windows::Win32::Foundation::{CloseHandle, BOOL, FILETIME, HANDLE};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Performance::*;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::{GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO};
use windows::Win32::System::Threading::GetSystemTimes;

pub const HISTORY: usize = 60;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Battery {
    pub pct: u8,
    pub charging: bool,
    pub on_ac: bool,
    pub secs_left: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Wifi {
    pub connected: bool,
    pub ssid: Option<String>,
    pub signal: u32,
    pub adapter: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bluetooth {
    pub present: bool,
    pub on: bool,
    pub radio: String,
    pub connected: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub cpu: f32,
    pub cpu_hist: VecDeque<f32>,
    pub cores: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub mem_hist: VecDeque<f32>,
    pub temps: Vec<(String, f32)>,
    pub battery: Option<Battery>,
    pub wifi: Option<Wifi>,
    pub bluetooth: Bluetooth,
    pub volume: Option<(f32, bool)>,
    pub uptime_secs: u64,
}

impl Stats {
    pub fn mem_pct(&self) -> f32 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f32 / self.mem_total as f32 * 100.0
        }
    }
    pub fn max_temp(&self) -> Option<f32> {
        self.temps.iter().map(|t| t.1).fold(None, |m, t| Some(m.map_or(t, |m: f32| m.max(t))))
    }
}

pub struct Sampler {
    prev_cpu: Option<(u64, u64)>,
    pdh_query: isize,
    pdh_temp: isize,
    wlan: Option<HANDLE>,
    audio: Option<IAudioEndpointVolume>,
    audio_at: Instant,
    bt_at: Instant,
    pub stats: Stats,
}

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl Sampler {
    /// Call on a COM-initialised thread (Core Audio).
    pub fn new() -> Self {
        let (mut q, mut c) = (0isize, 0isize);
        unsafe {
            if PdhOpenQueryW(None, 0, &mut q) == 0
                && PdhAddEnglishCounterW(q, w!("\\Thermal Zone Information(*)\\Temperature"), 0, &mut c) == 0
            {
                PdhCollectQueryData(q);
            } else {
                c = 0;
            }
        }
        let wlan = unsafe {
            let (mut ver, mut h) = (0u32, HANDLE::default());
            (WlanOpenHandle(2, None, &mut ver, &mut h) == 0).then_some(h)
        };
        let mut info = SYSTEM_INFO::default();
        unsafe { GetSystemInfo(&mut info) };
        let mut s = Sampler {
            prev_cpu: None,
            pdh_query: q,
            pdh_temp: c,
            wlan,
            audio: None,
            audio_at: Instant::now() - Duration::from_secs(3600),
            bt_at: Instant::now() - Duration::from_secs(3600),
            stats: Stats { cores: info.dwNumberOfProcessors, ..Default::default() },
        };
        s.sample();
        s
    }

    pub fn sample(&mut self) {
        self.sample_cpu();
        self.sample_mem();
        self.stats.temps = self.sample_temps();
        self.stats.battery = battery();
        self.stats.wifi = self.sample_wifi();
        if self.bt_at.elapsed() > Duration::from_secs(6) {
            self.stats.bluetooth = bluetooth();
            self.bt_at = Instant::now();
        }
        self.stats.volume = self.volume();
        self.stats.uptime_secs = unsafe { GetTickCount64() } / 1000;
    }

    fn sample_cpu(&mut self) {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
            return;
        }
        let (idle, total) = (ft(idle), ft(kernel) + ft(user)); // kernel time includes idle
        if let Some((pi, pt)) = self.prev_cpu {
            let dt = total.saturating_sub(pt);
            if dt > 0 {
                let busy = dt.saturating_sub(idle.saturating_sub(pi));
                self.stats.cpu = (busy as f32 / dt as f32 * 100.0).clamp(0.0, 100.0);
                push(&mut self.stats.cpu_hist, self.stats.cpu);
            }
        }
        self.prev_cpu = Some((idle, total));
    }

    fn sample_mem(&mut self) {
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
            self.stats.mem_total = m.ullTotalPhys;
            self.stats.mem_used = m.ullTotalPhys - m.ullAvailPhys;
            let pct = self.stats.mem_pct();
            push(&mut self.stats.mem_hist, pct);
        }
    }

    fn sample_temps(&mut self) -> Vec<(String, f32)> {
        if self.pdh_temp == 0 {
            return Vec::new();
        }
        unsafe {
            if PdhCollectQueryData(self.pdh_query) != 0 {
                return Vec::new();
            }
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.pdh_temp, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return Vec::new();
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.pdh_temp, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return Vec::new();
            }
            (0..count as usize)
                .filter_map(|i| {
                    let it = &*items.add(i);
                    let kelvin = it.FmtValue.Anonymous.doubleValue;
                    let name = it.szName.to_string().unwrap_or_default();
                    let name = name.rsplit('.').next().unwrap_or(&name).to_string();
                    (kelvin > 200.0).then_some((name, (kelvin - 273.15) as f32))
                })
                .collect()
        }
    }

    fn sample_wifi(&mut self) -> Option<Wifi> {
        let h = self.wlan?;
        unsafe {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(h, None, &mut list) != 0 || list.is_null() {
                return None;
            }
            let n = (*list).dwNumberOfItems as usize;
            let infos = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            let mut result = None;
            for info in infos {
                let mut wifi = Wifi { adapter: from_wide(&info.strInterfaceDescription), ..Default::default() };
                wifi.connected = info.isState == wlan_interface_state_connected;
                if wifi.connected {
                    let mut size = 0u32;
                    let mut data: *mut c_void = std::ptr::null_mut();
                    if WlanQueryInterface(
                        h,
                        &info.InterfaceGuid,
                        wlan_intf_opcode_current_connection,
                        None,
                        &mut size,
                        &mut data,
                        None,
                    ) == 0
                        && !data.is_null()
                    {
                        let attrs = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                        let assoc = &attrs.wlanAssociationAttributes;
                        let len = (assoc.dot11Ssid.uSSIDLength as usize).min(32);
                        let ssid = String::from_utf8_lossy(&assoc.dot11Ssid.ucSSID[..len]).to_string();
                        let profile = from_wide(&attrs.strProfileName);
                        // Windows 11 24H2 withholds the SSID without location access.
                        wifi.ssid = [ssid, profile].into_iter().find(|s| !s.trim().is_empty());
                        wifi.signal = assoc.wlanSignalQuality;
                        WlanFreeMemory(data);
                    }
                }
                let better = result.as_ref().is_none_or(|r: &Wifi| !r.connected && wifi.connected);
                if better {
                    result = Some(wifi);
                }
            }
            WlanFreeMemory(list as *const c_void);
            result
        }
    }

    fn endpoint(&mut self) -> Option<&IAudioEndpointVolume> {
        // The default device can change (headphones); re-resolve periodically.
        if self.audio.is_none() || self.audio_at.elapsed() > Duration::from_secs(10) {
            self.audio_at = Instant::now();
            self.audio = unsafe {
                CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .and_then(|e| e.GetDefaultAudioEndpoint(eRender, eConsole))
                    .and_then(|d| d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None))
                    .ok()
            };
        }
        self.audio.as_ref()
    }

    fn volume(&mut self) -> Option<(f32, bool)> {
        let ep = self.endpoint()?;
        unsafe {
            let v = ep.GetMasterVolumeLevelScalar().ok()?;
            let m = ep.GetMute().ok()?.as_bool();
            Some((v * 100.0, m))
        }
    }

    pub fn change_volume(&mut self, delta_pct: f32) {
        if let Some(ep) = self.endpoint() {
            unsafe {
                if let Ok(v) = ep.GetMasterVolumeLevelScalar() {
                    let _ = ep.SetMasterVolumeLevelScalar((v + delta_pct / 100.0).clamp(0.0, 1.0), &GUID::zeroed());
                    let _ = ep.SetMute(false, &GUID::zeroed());
                }
            }
        }
        self.stats.volume = self.volume();
    }

    pub fn toggle_mute(&mut self) {
        if let Some(ep) = self.endpoint() {
            unsafe {
                if let Ok(m) = ep.GetMute() {
                    let _ = ep.SetMute(!m.as_bool(), &GUID::zeroed());
                }
            }
        }
        self.stats.volume = self.volume();
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        unsafe {
            if self.pdh_query != 0 {
                PdhCloseQuery(self.pdh_query);
            }
            if let Some(h) = self.wlan {
                WlanCloseHandle(h, None);
            }
        }
    }
}

fn push(hist: &mut VecDeque<f32>, v: f32) {
    if hist.len() == HISTORY {
        hist.pop_front();
    }
    hist.push_back(v);
}

fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).ok()? };
    // 128 = no system battery, 255 = unknown.
    if s.BatteryFlag & 128 != 0 || s.BatteryFlag == 255 || s.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery {
        pct: s.BatteryLifePercent,
        charging: s.BatteryFlag & 8 != 0,
        on_ac: s.ACLineStatus == 1,
        secs_left: (s.BatteryLifeTime != u32::MAX).then_some(s.BatteryLifeTime),
    })
}

fn bluetooth() -> Bluetooth {
    let mut bt = Bluetooth::default();
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
        let mut radio = HANDLE::default();
        let Ok(find) = BluetoothFindFirstRadio(&params, &mut radio) else {
            return bt; // no radio, or radio turned off
        };
        let _ = BluetoothFindRadioClose(find);
        bt.present = true;
        let mut info = BLUETOOTH_RADIO_INFO { dwSize: size_of::<BLUETOOTH_RADIO_INFO>() as u32, ..Default::default() };
        if BluetoothGetRadioInfo(radio, &mut info) == 0 {
            bt.radio = from_wide(&info.szName);
        }
        bt.on = BluetoothIsConnectable(radio).as_bool();

        let search = BLUETOOTH_DEVICE_SEARCH_PARAMS {
            dwSize: size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
            fReturnAuthenticated: BOOL(1),
            fReturnRemembered: BOOL(1),
            fReturnUnknown: BOOL(0),
            fReturnConnected: BOOL(1),
            fIssueInquiry: BOOL(0),
            cTimeoutMultiplier: 0,
            hRadio: radio,
        };
        let mut dev = BLUETOOTH_DEVICE_INFO { dwSize: size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
        if let Ok(f) = BluetoothFindFirstDevice(&search, &mut dev) {
            loop {
                if dev.fConnected.as_bool() {
                    bt.connected.push(from_wide(&dev.szName));
                }
                dev = BLUETOOTH_DEVICE_INFO { dwSize: size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
                if BluetoothFindNextDevice(f, &mut dev).is_err() {
                    break;
                }
            }
            let _ = BluetoothFindDeviceClose(f);
        }
        bt.on |= !bt.connected.is_empty();
        let _ = CloseHandle(radio);
    }
    bt
}
