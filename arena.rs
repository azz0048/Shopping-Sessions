//! Block arena: one big huge-page-backed region carved into fixed-size blocks.
//! Buckets are chains of blocks (no realloc copies); freed blocks are reused by later phases
//! so most writes land on memory that is already faulted in.
use std::sync::Mutex;

pub struct Arena {
    block: usize,
    region: usize,
    state: Mutex<State>,
}
struct State {
    cur: usize,
    end: usize,
    free: Vec<usize>,
}

static mut ARENA: Option<Arena> = None;

#[allow(static_mut_refs)]
pub fn arena() -> &'static Arena {
    unsafe { ARENA.as_ref().unwrap() }
}

fn map_region(size: usize) -> usize {
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        );
        assert!(p != libc::MAP_FAILED, "arena mmap failed");
        libc::madvise(p, size, libc::MADV_HUGEPAGE);
        p as usize
    }
}

/// Must be called once before any bucket is used. Regions of `region` bytes are mapped on demand.
#[allow(static_mut_refs)]
pub fn init_arena(region: usize, block: usize) {
    let region = region.max(block * 64) / block * block;
    unsafe {
        ARENA = Some(Arena { block, region, state: Mutex::new(State { cur: 0, end: 0, free: Vec::new() }) });
    }
}

impl Arena {
    #[inline]
    pub fn block_bytes(&self) -> usize {
        self.block
    }
    pub fn alloc(&self) -> *mut u8 {
        let mut st = self.state.lock().unwrap();
        if let Some(p) = st.free.pop() {
            return p as *mut u8;
        }
        if st.cur + self.block > st.end {
            let r = map_region(self.region);
            st.cur = r;
            st.end = r + self.region;
        }
        let p = st.cur;
        st.cur += self.block;
        p as *mut u8
    }
    pub fn release(&self, blocks: &[usize]) {
        self.state.lock().unwrap().free.extend_from_slice(blocks);
    }
}

/// Append-only bucket made of arena blocks.
pub struct Bucket<T: Copy> {
    cur: *mut T,
    end: *mut T,
    start: *mut T,
    blocks: Vec<(*mut T, usize)>, // completed blocks (ptr, len)
    len: usize,
}
unsafe impl<T: Copy> Send for Bucket<T> {}
unsafe impl<T: Copy> Sync for Bucket<T> {}

impl<T: Copy> Bucket<T> {
    pub fn new() -> Self {
        Bucket {
            cur: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
            start: std::ptr::null_mut(),
            blocks: Vec::new(),
            len: 0,
        }
    }
    #[inline(always)]
    pub fn push(&mut self, v: T) {
        if self.cur == self.end {
            self.grow();
        }
        unsafe {
            self.cur.write(v);
            self.cur = self.cur.add(1);
        }
    }
    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        if !self.start.is_null() {
            let n = unsafe { self.cur.offset_from(self.start) as usize };
            self.blocks.push((self.start, n));
            self.len += n;
        }
        let a = arena();
        let p = a.alloc() as *mut T;
        let cap = a.block_bytes() / std::mem::size_of::<T>();
        self.start = p;
        self.cur = p;
        self.end = unsafe { p.add(cap) };
    }
    pub fn len(&self) -> usize {
        self.len + if self.start.is_null() { 0 } else { unsafe { self.cur.offset_from(self.start) as usize } }
    }
    /// Calls f on each contiguous slice.
    #[inline(always)]
    pub fn for_slices(&self, mut f: impl FnMut(&[T])) {
        for &(p, n) in &self.blocks {
            f(unsafe { std::slice::from_raw_parts(p, n) });
        }
        if !self.start.is_null() {
            let n = unsafe { self.cur.offset_from(self.start) as usize };
            f(unsafe { std::slice::from_raw_parts(self.start, n) });
        }
    }
    /// Return all blocks to the arena.
    pub fn release(&mut self) {
        let mut v: Vec<usize> = self.blocks.iter().map(|&(p, _)| p as usize).collect();
        if !self.start.is_null() {
            v.push(self.start as usize);
        }
        arena().release(&v);
        self.blocks.clear();
        self.start = std::ptr::null_mut();
        self.cur = std::ptr::null_mut();
        self.end = std::ptr::null_mut();
        self.len = 0;
    }
}
