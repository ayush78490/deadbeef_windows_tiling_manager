//! wallhaven.cc search API (https://wallhaven.cc/help/api).

use serde::Deserialize;

use super::http;
use crate::config::types::WallpaperConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Thumbs {
    pub small: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub id: String,
    /// Full-resolution image URL.
    pub path: String,
    #[serde(default)]
    pub resolution: String,
    #[serde(default)]
    pub file_size: u64,
    pub thumbs: Thumbs,
}

impl Item {
    pub fn extension(&self) -> &str {
        self.path.rsplit('.').next().filter(|e| e.len() <= 4).unwrap_or("jpg")
    }
    pub fn file_name(&self) -> String {
        format!("wallhaven-{}.{}", self.id, self.extension())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub last: u32,
    pub total: u64,
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
    meta: serde_json::Value,
}

pub fn search_url(cfg: &WallpaperConfig, query: &str, page: u32, atleast: &str) -> String {
    let mut url = format!(
        "https://wallhaven.cc/api/v1/search?q={}&categories={}&purity={}&sorting={}&page={}",
        http::encode(query),
        cfg.categories,
        cfg.purity,
        cfg.sorting,
        page.max(1)
    );
    if cfg.sorting == "toplist" {
        url += &format!("&topRange={}", cfg.top_range);
    }
    if !atleast.is_empty() {
        url += &format!("&atleast={atleast}");
    }
    if !cfg.apikey.is_empty() {
        url += &format!("&apikey={}", http::encode(&cfg.apikey));
    }
    url
}

pub fn search(cfg: &WallpaperConfig, query: &str, page: u32, atleast: &str) -> Result<Page, String> {
    let body = http::get(&search_url(cfg, query, page, atleast), |_, _| true)?;
    parse(&body)
}

pub fn parse(body: &[u8]) -> Result<Page, String> {
    let r: Response = serde_json::from_slice(body).map_err(|e| format!("unexpected wallhaven response: {e}"))?;
    // Numbers in `meta` are sometimes sent as strings.
    let num = |k: &str| -> u64 {
        match &r.meta[k] {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
            serde_json::Value::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        }
    };
    Ok(Page { last: num("last_page").max(1) as u32, total: num("total"), items: r.data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_response() {
        let body = br#"{"data":[{"id":"lygg6r","path":"https://w.wallhaven.cc/full/ly/wallhaven-lygg6r.jpg",
            "resolution":"4928x3280","file_size":1854551,"file_type":"image/jpeg",
            "thumbs":{"large":"https://th.wallhaven.cc/lg/ly/lygg6r.jpg","small":"https://th.wallhaven.cc/small/ly/lygg6r.jpg"}}],
            "meta":{"current_page":1,"last_page":4,"per_page":"24","total":93,"query":"mountains","seed":null}}"#;
        let p = parse(body).unwrap();
        assert_eq!((p.last, p.total), (4, 93));
        assert_eq!(p.items[0].file_name(), "wallhaven-lygg6r.jpg");
    }

    #[test]
    fn url() {
        let cfg = WallpaperConfig::default();
        let u = search_url(&cfg, "nord mountains", 2, "1920x1080");
        assert!(u.contains("q=nord+mountains") && u.contains("page=2") && u.contains("topRange=1M") && u.contains("atleast=1920x1080"));
    }
}
