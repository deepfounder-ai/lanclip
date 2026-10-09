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
fn default_retention_days() -> f64 { 2.0 }
fn default_max_storage_gb() -> f64 { 10.0 }

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
    /// History items older than this are deleted.
    #[serde(default = "default_retention_days")]
    pub retention_days: f64,
    /// When history exceeds this size, the oldest items are deleted.
    #[serde(default = "default_max_storage_gb")]
    pub max_storage_gb: f64,
    /// Devices this one is paired with, and the key to use when talking to each of them.
    #[serde(default)]
    pub paired: std::collections::BTreeMap<String, PairedPeer>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PairedPeer {
    pub name: String,
    pub key: String,
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
            retention_days: default_retention_days(),
            max_storage_gb: default_max_storage_gb(),
            paired: Default::default(),
        });
        let mut store = Store { dir: dir.to_path_buf(), items_dir, settings, items: vec![] };
        let items = read_json::<Vec<Item>>(&dir.join("items.json")).unwrap_or_default();
        store.items = items.into_iter().filter(|it| store.content_path(it).exists()).collect();
        store.settings.max_items = store.settings.max_items.max(default_max());
        store.save_settings();
        // Remove leftovers not referenced by the history (e.g. interrupted transfers).
        if let Ok(entries) = fs::read_dir(&store.items_dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !store.items.iter().any(|it| it.id == name) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        store.prune();
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

    /// Retention: delete items older than retention_days, then the oldest items while history is
    /// over max_storage_gb or max_items. The size/count limits never touch the newest batch
    /// (it may still be arriving). Returns true if anything was deleted.
    pub fn prune(&mut self) -> bool {
        let before = self.items.len();
        let cutoff = now_ms().saturating_sub((self.settings.retention_days.max(0.01) * 86_400_000.0) as u64);
        let (expired, kept): (Vec<Item>, Vec<Item>) = std::mem::take(&mut self.items).into_iter().partition(|it| it.ts < cutoff);
        self.items = kept;
        for it in expired {
            let _ = fs::remove_dir_all(self.items_dir.join(&it.id));
        }

        let max_bytes = (self.settings.max_storage_gb.max(0.1) * 1024.0 * 1024.0 * 1024.0) as u64;
        let mut total: u64 = self.items.iter().map(|it| it.size).sum();
        let keep = self.items.first().map(|it| it.batch.clone()).unwrap_or_default();
        let mut i = self.items.len();
        while (total > max_bytes || self.items.len() > self.settings.max_items) && i > 0 {
            i -= 1;
            if self.items[i].batch == keep { continue; }
            let old = self.items.remove(i);
            total -= old.size;
            let _ = fs::remove_dir_all(self.items_dir.join(&old.id));
        }
        let changed = self.items.len() != before;
        if changed {
            self.save_items();
        }
        changed
    }

    pub fn used_bytes(&self) -> u64 {
        self.items.iter().map(|it| it.size).sum()
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
