//! App index from the shell's AppsFolder (`shell:AppsFolder`) — the same list
//! as Start → All apps: Win32 programs and Store/UWP apps alike — plus a
//! usage history so frequently launched apps rank first.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    IEnumShellItems, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SHGetKnownFolderItem,
    BHID_EnumItems, FOLDERID_AppsFolder, KF_FLAG_DEFAULT, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    SIIGBF_ICONONLY,
};

use crate::ui::image::{self, Pixels};

#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    /// AppsFolder parsing name: an AUMID (Store apps) or a path/known-folder
    /// path (desktop apps). Launch with `shell:AppsFolder\<target>`.
    pub target: String,
}

static APPS: Mutex<Option<Vec<App>>> = Mutex::new(None);
static ICONS: Mutex<Option<HashMap<String, Option<Pixels>>>> = Mutex::new(None);

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let p = item.GetDisplayName(kind).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Enumerates AppsFolder. COM must be initialised. Takes ~0.1–0.3 s.
pub fn enumerate() -> Vec<App> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<_, IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return out;
        };
        let mut batch: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
        loop {
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            for slot in batch.iter_mut().take(fetched as usize) {
                if let Some(item) = slot.take() {
                    let (Some(name), Some(target)) =
                        (display_name(&item, SIGDN_NORMALDISPLAY), display_name(&item, SIGDN_PARENTRELATIVEPARSING))
                    else {
                        continue;
                    };
                    // Skip uninstallers, docs and web links that clutter Start.
                    let lower = name.to_lowercase();
                    let t = target.to_lowercase();
                    if lower.starts_with("uninstall") || t.ends_with(".url") || t.ends_with(".chm") || t.ends_with(".txt") || t.ends_with(".html") || t.starts_with("http") {
                        continue;
                    }
                    out.push(App { name, target });
                }
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name == b.name && a.target == b.target);
    out
}

pub fn cached() -> Option<Vec<App>> {
    APPS.lock().unwrap().clone()
}

pub fn store(apps: Vec<App>) {
    *APPS.lock().unwrap() = Some(apps);
}

/// Icon for an AppsFolder target (cached across launcher sessions).
pub fn icon(target: &str, size: i32) -> Option<Pixels> {
    if let Some(hit) = ICONS.lock().unwrap().get_or_insert_with(HashMap::new).get(target) {
        return hit.clone();
    }
    let px = unsafe {
        (|| {
            let path = HSTRING::from(format!("shell:AppsFolder\\{target}"));
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None).ok()?;
            let hbm = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY).ok()?;
            let px = image::from_hbitmap(hbm);
            let _ = DeleteObject(hbm);
            px
        })()
    };
    ICONS.lock().unwrap().get_or_insert_with(HashMap::new).insert(target.to_string(), px.clone());
    px
}

pub fn icon_cached(target: &str) -> Option<Option<Pixels>> {
    ICONS.lock().unwrap().as_ref().and_then(|m| m.get(target).cloned())
}

// ───────────────────────────── history ─────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Use {
    pub count: u32,
    pub last: u64,
}

pub type History = HashMap<String, Use>;

fn history_path() -> PathBuf {
    crate::config::Paths::discover().root.join("launcher-history.json")
}

pub fn load_history() -> History {
    std::fs::read(history_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn record(key: &str) {
    let mut h = load_history();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let e = h.entry(key.to_string()).or_default();
    e.count += 1;
    e.last = now;
    if let Ok(json) = serde_json::to_vec(&h) {
        let _ = std::fs::write(history_path(), json);
    }
}

/// Ranking bonus from usage: frequency, decayed by recency.
pub fn bonus(h: &History, key: &str) -> i32 {
    let Some(u) = h.get(key) else { return 0 };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = now.saturating_sub(u.last) / 86_400;
    let recency = if days < 1 { 12 } else if days < 7 { 6 } else { 0 };
    (u.count.min(20) as i32) * 3 + recency
}
