use alloc::alloc::Layout;
use core::ops::Range;

use js_sys::WebAssembly;

use super::Error;

#[derive(Debug)]
pub struct AllocState {
    table: WebAssembly::Table,
    // There is no "allocator" for tables, so we need to track free entries ourselves.
    // list of (table_offset, size) for each free table entry
    free_tables: Vec<Range<u32>>,
}

#[derive(Debug)]
pub struct RawAllocEntry {
    memory_start: *const u8,
    memory_size: u32,
    table_start: u32,
    table_size: u32,
}
impl RawAllocEntry {
    pub fn table_start(&self) -> u32 {
        self.table_start
    }
    pub fn memory_start(&self) -> u32 {
        self.memory_start as u32
    }
}

unsafe impl Send for RawAllocEntry {}

unsafe impl Sync for RawAllocEntry {}

const DEFAULT_ALIGNMENT: usize = core::mem::align_of::<u32>();

impl AllocState {
    pub fn new(table: WebAssembly::Table) -> Self {
        AllocState {
            table,
            free_tables: Vec::new(),
        }
    }
    pub fn alloc(&mut self, bytes: u32, table_size: u32) -> Result<RawAllocEntry, Error> {
        let mem = unsafe {
            alloc::alloc::alloc(Layout::from_size_align(bytes as usize, DEFAULT_ALIGNMENT).unwrap())
        };

        let table_offset = self.try_find_free_table(table_size).unwrap_or_else(|| {
            let offset = self.table.length();
            self.table
                .grow(table_size as u32)
                .expect("Failed to grow WebAssembly Table");
            offset
        });

        Ok(RawAllocEntry {
            memory_start: mem,
            memory_size: bytes,
            table_start: table_offset,
            table_size,
        })
    }

    pub fn free(&mut self, entry: RawAllocEntry) {
        unsafe {
            alloc::alloc::dealloc(
                entry.memory_start as *mut u8,
                Layout::from_size_align(entry.memory_size as usize, DEFAULT_ALIGNMENT).unwrap(),
            );
        }
        self.push_free_table(entry.table_start..entry.table_start + entry.table_size);
    }

    fn push_free_table(&mut self, range: Range<u32>) {
        self.free_tables.push(range);
        self.free_tables.sort_by_key(|r| r.start);
        // Merge adjacent free ranges
        let mut merged: Vec<Range<u32>> = Vec::new();
        for range in &self.free_tables {
            if let Some(last) = merged.last_mut() {
                debug_assert!(last.start <= range.start);

                if last.end == range.start {
                    last.end = last.end.max(range.end);
                    continue;
                }
            }
            merged.push(range.clone());
        }
        self.free_tables = merged;
    }

    fn try_find_free_table(&mut self, size: u32) -> Option<u32> {
        // find exactly fitting entry
        let mut found_entry: Option<u32> = None;
        // if not found, find the most closely fitting entry
        let mut closest_entry: Option<(u32, u32)> = None;

        for (i, entry) in self.free_tables.iter().enumerate() {
            if entry.len() as u32 == size {
                found_entry = Some(i as u32);
                break;
            } else if entry.len() as u32 > size {
                // if entry is smaller than closest found, or if no closest found yet
                let update = !matches!(closest_entry, Some((_, closest_size)) if entry.len() as u32>= closest_size );
                if update {
                    closest_entry = Some((i as u32, entry.len() as u32));
                }
            }
        }
        if found_entry.is_none() {
            if let Some((index, _)) = closest_entry {
                found_entry = Some(index);
            }
        }

        let index = found_entry?;

        let entry = &self.free_tables[index as usize];
        let offset = entry.start;

        if entry.len() as u32 > size {
            let new_entry = entry.start + size..entry.end;
            self.free_tables[index as usize] = new_entry;
        }

        Some(offset)
    }
}
