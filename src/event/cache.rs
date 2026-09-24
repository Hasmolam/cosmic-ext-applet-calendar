// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::event::CalendarEvent;

#[derive(Debug, Clone)]
struct CacheEntry {
    events: Vec<CalendarEvent>,
    inserted_at: Instant,
}

/// In-memory cache for calendar events keyed by `(year, month)`.
/// Uses an LRU (Least Recently Used) eviction strategy with TTL to ensure fresh data.
#[derive(Debug, Clone)]
pub struct EventCache {
    months: HashMap<(i16, i8), CacheEntry>,
    access_order: VecDeque<(i16, i8)>,
    max_months: usize,
    ttl: Duration,
}

impl Default for EventCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EventCache {
    pub const DEFAULT_MAX_MONTHS: usize = 6;
    pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Self::with_capacity_and_ttl(Self::DEFAULT_MAX_MONTHS, Self::DEFAULT_TTL)
    }

    pub fn with_capacity(max_months: usize) -> Self {
        Self::with_capacity_and_ttl(max_months, Self::DEFAULT_TTL)
    }

    pub fn with_capacity_and_ttl(max_months: usize, ttl: Duration) -> Self {
        Self {
            months: HashMap::new(),
            access_order: VecDeque::new(),
            max_months: max_months.max(1),
            ttl,
        }
    }

    /// Retrieves cached events for the given year and month.
    /// Updates the LRU access order if found.
    pub fn get(&mut self, year: i16, month: i8) -> Option<&Vec<CalendarEvent>> {
        let key = (year, month);
        if self.months.contains_key(&key) {
            // Move key to the back of the queue (most recently used)
            if let Some(pos) = self.access_order.iter().position(|&k| k == key) {
                self.access_order.remove(pos);
            }
            self.access_order.push_back(key);
            self.months.get(&key).map(|e| &e.events)
        } else {
            None
        }
    }

    /// Returns true if the given month is not cached OR if its cached entry has exceeded the TTL.
    pub fn is_stale(&self, year: i16, month: i8) -> bool {
        match self.months.get(&(year, month)) {
            Some(entry) => entry.inserted_at.elapsed() >= self.ttl,
            None => true,
        }
    }

    /// Checks if the given year and month is present in the cache without altering LRU order.
    pub fn contains(&self, year: i16, month: i8) -> bool {
        self.months.contains_key(&(year, month))
    }

    /// Inserts events for a given year and month into the cache.
    /// Evicts the oldest month if capacity is exceeded.
    pub fn insert(&mut self, year: i16, month: i8, events: Vec<CalendarEvent>) {
        let key = (year, month);

        if let Some(pos) = self.access_order.iter().position(|&k| k == key) {
            self.access_order.remove(pos);
        } else if self.months.len() >= self.max_months
            && let Some(oldest) = self.access_order.pop_front()
        {
            self.months.remove(&oldest);
        }

        self.access_order.push_back(key);
        self.months.insert(
            key,
            CacheEntry {
                events,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Invalidates cached events for a specific year and month.
    pub fn invalidate(&mut self, year: i16, month: i8) {
        let key = (year, month);
        self.months.remove(&key);
        if let Some(pos) = self.access_order.iter().position(|&k| k == key) {
            self.access_order.remove(pos);
        }
    }

    /// Clears the entire cache.
    pub fn clear(&mut self) {
        self.months.clear();
        self.access_order.clear();
    }

    /// Returns the number of cached months.
    pub fn len(&self) -> usize {
        self.months.len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.months.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn make_test_event(id: &str, year: i16, month: i8, day: i8) -> CalendarEvent {
        let tz = jiff::tz::TimeZone::UTC;
        let d = date(year, month, day).to_zoned(tz).unwrap();
        CalendarEvent {
            id: id.to_string(),
            summary: format!("Event {id}"),
            start: d.clone(),
            end: d,
            is_all_day: true,
            location: None,
            url: None,
        }
    }

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = EventCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.get(2026, 4), None);

        let events = vec![make_test_event("e1", 2026, 4, 15)];
        cache.insert(2026, 4, events.clone());

        assert_eq!(cache.len(), 1);
        assert!(cache.contains(2026, 4));

        let retrieved = cache.get(2026, 4).unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].id, "e1");
    }

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache = EventCache::with_capacity(2);

        cache.insert(2026, 1, vec![make_test_event("m1", 2026, 1, 1)]);
        cache.insert(2026, 2, vec![make_test_event("m2", 2026, 2, 1)]);
        assert_eq!(cache.len(), 2);

        // Access month 1 to make it most recently used; month 2 becomes least recently used
        assert!(cache.get(2026, 1).is_some());

        // Insert month 3 -> should evict month 2!
        cache.insert(2026, 3, vec![make_test_event("m3", 2026, 3, 1)]);
        assert_eq!(cache.len(), 2);

        assert!(cache.contains(2026, 1));
        assert!(!cache.contains(2026, 2));
        assert!(cache.contains(2026, 3));
    }

    #[test]
    fn test_cache_invalidation() {
        let mut cache = EventCache::new();
        cache.insert(2026, 5, vec![make_test_event("m5", 2026, 5, 1)]);
        assert!(cache.contains(2026, 5));

        cache.invalidate(2026, 5);
        assert!(!cache.contains(2026, 5));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_cache_clear() {
        let mut cache = EventCache::new();
        cache.insert(2026, 1, vec![]);
        cache.insert(2026, 2, vec![]);
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_stale_ttl() {
        let mut cache = EventCache::with_capacity_and_ttl(2, Duration::from_millis(50));
        assert!(cache.is_stale(2026, 6));

        cache.insert(2026, 6, vec![make_test_event("m6", 2026, 6, 1)]);
        assert!(!cache.is_stale(2026, 6));

        std::thread::sleep(Duration::from_millis(60));
        assert!(cache.is_stale(2026, 6));
        // Still available in cache for stale-while-revalidate
        assert!(cache.get(2026, 6).is_some());
    }
}
