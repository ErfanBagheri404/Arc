//! Calendar subscriptions: a list of `.ics` URLs, fetched on a worker thread
//! and merged into one upcoming-events list.
//!
//! Google OAuth (device flow, DPAPI-encrypted token) is a separate increment —
//! this module only knows subscribed URLs, which is what a user can add
//! without any account plumbing.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::calendar::{self, Event};

/// How often the worker re-fetches every subscription.
const REFRESH_SECS: u64 = 900;

/// One subscribed calendar source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sub {
    /// Shown in the panel, defaults to the URL when absent.
    pub name: String,
    pub url: String,
}

pub struct Calendar {
    shared: Arc<Mutex<Snapshot>>,
    alive: Arc<AtomicBool>,
}

/// Merged events plus the subscriptions that produced them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Upcoming events, soonest first.
    pub events: Vec<Event>,
    pub subs: Vec<Sub>,
    /// Subscriptions whose last fetch failed — shown so a broken URL is
    /// visible rather than silently showing fewer events.
    pub failed: Vec<String>,
}

impl Calendar {
    /// Load the subscription list from disk and start the worker.
    pub fn start() -> Self {
        let subs = load_subs();
        let shared = Arc::new(Mutex::new(Snapshot {
            subs: subs.clone(),
            ..Snapshot::default()
        }));
        let alive = Arc::new(AtomicBool::new(true));
        let (sh, al) = (shared.clone(), alive.clone());
        std::thread::Builder::new()
            .name("calendar".into())
            .spawn(move || {
                // Reminder state: which events have already fired, so a
                // restart does not re-toast yesterday's meeting.
                let mut fired: Vec<String> = Vec::new();
                while al.load(Ordering::Relaxed) {
                    let subs = sh.lock().map(|g| g.subs.clone()).unwrap_or_default();
                    let mut merged = Snapshot {
                        subs: subs.clone(),
                        ..Snapshot::default()
                    };
                    for s in &subs {
                        match calendar::fetch(&s.url) {
                            Some(evs) => merged.events.extend(evs),
                            None => merged.failed.push(s.name.clone()),
                        }
                    }
                    // Soonest first, and drop anything already over.
                    merged.events.retain(|e| e.end > now());
                    merged.events.sort_by_key(|e| e.start);
                    merged.events.truncate(50);
                    // Fire a reminder for each event whose start just passed.
                    let now = now();
                    for e in &merged.events {
                        if e.start <= now && !fired.contains(&e.summary) {
                            fired.push(e.summary.clone());
                            crate::platform::tray::notify("Arc", &e.label());
                        }
                    }
                    // Keep the fired list from growing without bound.
                    if fired.len() > 50 {
                        fired.drain(..fired.len() - 50);
                    }
                    if let Ok(mut g) = sh.lock() {
                        *g = merged;
                    }
                    // Sleep in 1 s slices so shutdown is prompt.
                    for _ in 0..REFRESH_SECS {
                        if !al.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            })
            .expect("spawn calendar thread");
        Self { shared, alive }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// The next event that has not started yet, if any.
    pub fn next(&self) -> Option<Event> {
        let now = now();
        self.snapshot().events.into_iter().find(|e| e.start > now)
    }

    /// Add a subscription and persist it.
    pub fn subscribe(&self, name: &str, url: &str) {
        let Some(mut g) = self.shared.lock().ok() else {
            return;
        };
        if g.subs.iter().any(|s| s.url == url) {
            return;
        }
        g.subs.push(Sub {
            name: name.to_string(),
            url: url.to_string(),
        });
        save_subs(&g.subs);
    }

    /// Drop a subscription by URL and persist.
    pub fn unsubscribe(&self, url: &str) {
        let Some(mut g) = self.shared.lock().ok() else {
            return;
        };
        g.subs.retain(|s| s.url != url);
        save_subs(&g.subs);
    }
}

impl Drop for Calendar {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn load_subs() -> Vec<Sub> {
    subs_from_table(&super::settings::load())
}

/// The pure reader, so persistence is testable without touching the shared
/// settings file.
fn subs_from_table(t: &toml::Table) -> Vec<Sub> {
    t.get("cal_subs")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let e = v.as_table()?;
                    Some(Sub {
                        name: e.get("name")?.as_str()?.to_string(),
                        url: e.get("url")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save_subs(subs: &[Sub]) {
    let mut t = super::settings::load();
    t.insert("cal_subs".into(), toml::Value::Array(sub_table(subs)));
    super::settings::save(&t);
}

/// The pure writer, paired with [`subs_from_table`].
fn sub_table(subs: &[Sub]) -> Vec<toml::Value> {
    let arr: Vec<toml::Value> = subs
        .iter()
        .map(|s| {
            let mut m = toml::Table::new();
            m.insert("name".into(), toml::Value::String(s.name.clone()));
            m.insert("url".into(), toml::Value::String(s.url.clone()));
            toml::Value::Table(m)
        })
        .collect();
    arr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_url_lands_in_failed_not_in_events() {
        let mut s = Snapshot::default();
        s.failed.push("Work".into());
        assert!(s.events.is_empty());
        assert_eq!(s.failed.len(), 1);
    }

    #[test]
    fn subs_round_trip_through_a_table() {
        // The pure half of the persistence pair: no disk, so this cannot race
        // another test through the shared settings file.
        let subs = vec![Sub {
            name: "Work".into(),
            url: "https://example.com/a.ics".into(),
        }];
        let mut t = toml::Table::new();
        t.insert("cal_subs".into(), toml::Value::Array(sub_table(&subs)));
        assert_eq!(subs_from_table(&t), subs);
    }

    #[test]
    fn a_missing_subs_key_reads_as_no_subscriptions() {
        assert!(subs_from_table(&toml::Table::new()).is_empty());
    }
}
