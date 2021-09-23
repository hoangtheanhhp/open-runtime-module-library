use codec::Encode;
use parking_lot::RwLock;
use sp_state_machine::StorageKey;
use sp_storage::ChildInfo;
use std::{collections::HashMap, sync::Arc, time::Instant};

#[derive(PartialEq, Eq)]
enum AccessType {
	None,
	Redundant,
	Important,
}

impl Default for AccessType {
	fn default() -> Self {
		AccessType::None
	}
}

impl AccessType {
	fn is_important(&self) -> bool {
		*self == AccessType::Important
	}
	fn mark_important(&mut self) {
		*self = AccessType::Important;
	}
}

#[derive(Default)]
struct AccessInfo {
	pub read: AccessType,
	pub written: AccessType,
}

impl AccessInfo {
	fn read(redundant: bool) -> Self {
		let read = if redundant {
			AccessType::Redundant
		} else {
			AccessType::Important
		};
		Self {
			read,
			written: AccessType::None,
		}
	}

	fn written(redundant: bool) -> Self {
		let written = if redundant {
			AccessType::Redundant
		} else {
			AccessType::Important
		};
		Self {
			read: AccessType::Redundant,
			written,
		}
	}
}

#[derive(Default, Debug)]
struct AccessReport {
	pub read: u32,
	pub written: u32,
}

pub struct BenchTracker {
	instant: RwLock<Instant>,
	depth: RwLock<u32>,
	redundant: RwLock<Instant>,
	results: RwLock<Vec<u128>>,
	main_keys: RwLock<HashMap<StorageKey, AccessInfo>>,
	child_keys: RwLock<HashMap<StorageKey, HashMap<StorageKey, AccessInfo>>>,
}

impl BenchTracker {
	pub fn new() -> Self {
		BenchTracker {
			instant: RwLock::new(Instant::now()),
			depth: RwLock::new(0),
			redundant: RwLock::new(Instant::now()),
			results: RwLock::new(Vec::new()),
			main_keys: RwLock::new(HashMap::new()),
			child_keys: RwLock::new(HashMap::new()),
		}
	}

	pub fn instant(&self) {
		*self.instant.write() = Instant::now();
	}

	pub fn elapsed(&self) -> u128 {
		self.instant.read().elapsed().as_nanos()
	}

	pub fn is_redundant(&self) -> bool {
		*self.depth.read() > 1
	}

	pub fn reading_key(&self, key: StorageKey) {
		let redundant = self.is_redundant();
		let main_keys = &mut *self.main_keys.write();
		match main_keys.get_mut(&key) {
			Some(info) => {
				if redundant {
					return;
				}
				if info.written.is_important() {
					return;
				}
				info.read.mark_important();
			}
			None => {
				main_keys.insert(key, AccessInfo::read(redundant));
			}
		};
	}

	pub fn reading_child_key(&self, child_info: &ChildInfo, key: StorageKey) {
		let redundant = self.is_redundant();
		let child_keys = &mut *self.child_keys.write();
		let storage_key = child_info.storage_key().to_vec();
		match child_keys.get_mut(&storage_key) {
			Some(reads) => {
				match reads.get_mut(&key) {
					Some(info) => {
						if redundant {
							return;
						}
						if info.written.is_important() {
							return;
						}
						info.read.mark_important();
					}
					None => {
						reads.insert(key, AccessInfo::read(redundant));
					}
				};
			}
			None => {
				let mut reads = HashMap::<StorageKey, AccessInfo>::new();
				reads.insert(key, AccessInfo::read(redundant));
				child_keys.insert(storage_key, reads);
			}
		};
	}

	pub fn changing_key(&self, key: StorageKey) {
		let redundant = self.is_redundant();
		let main_keys = &mut *self.main_keys.write();
		match main_keys.get_mut(&key) {
			Some(info) => {
				if redundant {
					return;
				}
				info.written.mark_important();
			}
			None => {
				main_keys.insert(key, AccessInfo::written(redundant));
			}
		};
	}

	pub fn changing_child_key(&self, child_info: &ChildInfo, key: StorageKey) {
		let redundant = self.is_redundant();
		let child_keys = &mut *self.child_keys.write();
		let storage_key = child_info.storage_key().to_vec();
		match child_keys.get_mut(&storage_key) {
			Some(changes) => {
				match changes.get_mut(&key) {
					Some(info) => {
						if redundant {
							return;
						}
						info.written.mark_important();
					}
					None => {
						changes.insert(key, AccessInfo::written(redundant));
					}
				};
			}
			None => {
				let mut changes = HashMap::<StorageKey, AccessInfo>::new();
				changes.insert(key, AccessInfo::written(redundant));
				child_keys.insert(storage_key, changes);
			}
		};
	}

	pub fn read_written_keys(&self) -> Vec<u8> {
		let mut summary = HashMap::<StorageKey, AccessReport>::new();

		self.main_keys.read().iter().for_each(|(key, info)| {
			let prefix = key[0..32].to_vec();
			if let Some(report) = summary.get_mut(&prefix) {
				if info.read.is_important() {
					report.read += 1;
				}
				if info.written.is_important() {
					report.written += 1;
				}
			} else {
				let mut report = AccessReport::default();
				if info.read.is_important() {
					report.read += 1;
				}
				if info.written.is_important() {
					report.written += 1;
				}
				if report.read + report.written > 0 {
					summary.insert(prefix, report);
				}
			}
		});

		self.child_keys.read().iter().for_each(|(prefix, keys)| {
			keys.iter().for_each(|(key, info)| {
				let prefix = [prefix.clone(), key.clone()].concat()[0..32].to_vec();
				if let Some(report) = summary.get_mut(&prefix) {
					if info.read.is_important() {
						report.read += 1;
					}
					if info.written.is_important() {
						report.written += 1;
					}
				} else {
					let mut report = AccessReport::default();
					if info.read.is_important() {
						report.read += 1;
					}
					if info.written.is_important() {
						report.written += 1;
					}
					if report.read + report.written > 0 {
						summary.insert(prefix, report);
					}
				}
			});
		});

		summary
			.into_iter()
			.map(|(prefix, report)| (prefix, report.read, report.written))
			.collect::<Vec<(StorageKey, u32, u32)>>()
			.encode()
	}

	pub fn before_block(&self) {
		let timestamp = Instant::now();

		let mut depth = self.depth.write();

		if *depth == 0 {
			*depth = 1;
			return;
		}

		if *depth == 1 {
			*self.redundant.write() = timestamp;
		}

		*depth += 1;
	}

	pub fn after_block(&self) {
		let mut depth = self.depth.write();
		if *depth == 2 {
			let redundant = self.redundant.read();
			let elapsed = redundant.elapsed().as_nanos();
			self.results.write().push(elapsed);
		}
		*depth -= 1;
	}

	pub fn redundant_time(&self) -> u128 {
		assert!(*self.depth.read() == 0, "benchmark in progress");

		let mut elapsed = 0u128;

		self.results.read().iter().for_each(|x| {
			elapsed = elapsed.saturating_add(*x);
		});

		elapsed
	}

	pub fn reset_storage_tracker(&self) {
		self.main_keys.write().clear();
		self.child_keys.write().clear();
	}

	pub fn reset_redundant(&self) {
		*self.depth.write() = 0;
		self.results.write().clear();
	}
}

sp_externalities::decl_extension! {
	pub struct BenchTrackerExt(Arc<BenchTracker>);
}
