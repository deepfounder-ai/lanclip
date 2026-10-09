// Persistent settings and item history (items.json + items/<id>/<name>).
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Origin {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub kind: String, // text | image | file
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub ts: u64,
    pub origin: Origin,
    #[serde(default)]
    pub batch: String,
    #[serde(default = "one")]
    pub batch_total: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn one() -> u32 { 1 }
fn is_false(b: &bool) -> bool { !*b }
fn default_max() -> usize { 500 }

/// Item metadata as sent by peers and UIs (header x-lanclip-meta).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Meta {
    pub id: Option<String>,
    pub kind: Option<String>,
    pub name: Option<String>,
    pub mime: Option<String>,
    pub ts: Option<f64>,
    pub origin: Option<Origin>,
    pub batch: Option<String>,
    pub batch_total: Option<f64>,
    pub rel_path: Option<String>,
    pub origin_name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub id: String,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default)]
    pub manual_peers: Vec<String>,
    #[serde(default = "default_max")]
    pub max_items: usize,
}

pub struct Store {
    pub dir: PathBuf,
    pub items_dir: PathBuf,
    pub settings: Settings,
    pub items: Vec<Item>, // newest first
}

pub struct Batch {
    pub total: u32,
    pub items: Vec<Item>, // oldest first
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let items_dir = dir.join("items");
        let _ = fs::create_dir_all(&items_dir);
        let settings = read_json::<Settings>(&dir.join("settings.json")).unwrap_or_else(|| Settings {
            id: new_id(),
            name: gethostname::gethostname().to_string_lossy().into_owned(),
            key: random_key(),
            auto_sync: false,
            manual_peers: vec![],
            max_items: default_max(),
        });
        let mut store = Store { dir: dir.to_path_buf(), items_dir, settings, items: vec![] };
        let items = read_json::<Vec<Item>>(&dir.join("items.json")).unwrap_or_default();
        store.items = items.into_iter().filter(|it| store.content_path(it).exists()).collect();
        store.settings.max_items = store.settings.max_items.max(default_max());
        store.save_settings();
        store
    }

    pub fn save_settings(&self) {
        write_json(&self.dir.join("settings.json"), &self.settings);
    }

    pub fn save_items(&self) {
        write_json(&self.dir.join("items.json"), &self.items);
    }

    pub fn content_path(&self, item: &Item) -> PathBuf {
        self.items_dir.join(&item.id).join(safe_name(&item.name))
    }

    pub fn get(&self, id: &str) -> Option<Item> {
        self.items.iter().find(|it| it.id == id).cloned()
    }

    pub fn insert(&mut self, item: Item) {
        self.items.insert(0, item);
        self.prune();
        self.save_items();
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let Some(idx) = self.items.iter().position(|it| it.id == id) else { return false };
        let item = self.items.remove(idx);
        let _ = fs::remove_dir_all(self.items_dir.join(&item.id));
        self.save_items();
        true
    }

    pub fn clear(&mut self) {
        for it in &self.items {
            let _ = fs::remove_dir_all(self.items_dir.join(&it.id));
        }
        self.items.clear();
        self.save_items();
    }

    /// Keep at most max_items, but never prune the newest batch (it may still be arriving).
    fn prune(&mut self) {
        let keep = self.items.first().map(|it| it.batch.clone()).unwrap_or_default();
        let mut i = self.items.len();
        while self.items.len() > self.settings.max_items && i > 0 {
            i -= 1;
            if self.items[i].batch == keep { continue; }
            let old = self.items.remove(i);
            let _ = fs::remove_dir_all(self.items_dir.join(&old.id));
        }
    }

    /// The most recent batch: what "Paste from LAN" pastes.
    pub fn latest_batch(&self) -> Option<Batch> {
        let newest = self.items.first()?;
        let mut items: Vec<Item> = self.items.iter().filter(|it| it.batch == newest.batch).cloned().collect();
        items.reverse();
        Some(Batch { total: newest.batch_total.max(1), items })
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) {
    let tmp = path.with_extension("tmp");
    if let Ok(data) = serde_json::to_vec_pretty(value) {
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}
