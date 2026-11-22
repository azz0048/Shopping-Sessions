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
    free: Vec<(usize, u32)>,
    ptrs: Vec<usize>, // block id -> address
}

static mut ARENA: Option<Arena> = None;

thread_local! {
    static TL_BLOCKS: std::cell::RefCell<Vec<(usize, u32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[allow(static_mut_refs)]
pub fn arena() -> &'static Arena {
    unsafe { ARENA.as_ref().unwrap() }
}

#[cfg(windows)]
fn map_region(size: usize) -> usize {
    // Local dev/test builds only; the scored build is Linux.
    unsafe extern "system" {
        fn VirtualAlloc(addr: *mut u8, size: usize, ty: u32, protect: u32) -> *mut u8;
    }
    let p = unsafe { VirtualAlloc(std::ptr::null_mut(), size, 0x3000, 0x04) };
    assert!(!p.is_null(), "arena VirtualAlloc failed");
    if std::env::var_os("SS_PRETOUCH").is_some() {
        for i in (0..size).step_by(4096) {
            unsafe { std::ptr::write_volatile(p.add(i), 0) };
        }
    }
    p as usize
}

#[cfg(unix)]
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
        ARENA = Some(Arena { block, region, state: Mutex::new(State { cur: 0, end: 0, free: Vec::new(), ptrs: Vec::new() }) });
    }
}

impl Arena {
    #[inline]
    pub fn block_bytes(&self) -> usize {
        self.block
    }
    /// Returns (address, block id). Blocks come from a small per-thread cache that is refilled
    /// in batches (freed blocks first, then fresh ones), so threads rarely touch the lock.
    pub fn alloc(&self) -> (*mut u8, u32) {
        TL_BLOCKS.with(|c| {
            let mut c = c.borrow_mut();
            if let Some((p, id)) = c.pop() {
                return (p as *mut u8, id);
            }
            let mut st = self.state.lock().unwrap();
            for _ in 0..32 {
                if let Some(x) = st.free.pop() {
                    c.push(x);
                    continue;
                }
                if st.cur + self.block > st.end {
                    let r = map_region(self.region);
                    st.cur = r;
                    st.end = r + self.region;
                }
                let p = st.cur;
                st.cur += self.block;
                let id = st.ptrs.len() as u32;
                st.ptrs.push(p);
                c.push((p, id));
            }
            // hand out the lowest address first so a thread's blocks stay contiguous
            c.reverse();
            let (p, id) = c.pop().unwrap();
            (p as *mut u8, id)
        })
    }
    /// Return this thread's cached, unused blocks to the shared free list.
    pub fn flush_thread_cache(&self) {
        TL_BLOCKS.with(|c| {
            let mut c = c.borrow_mut();
            if !c.is_empty() {
                self.state.lock().unwrap().free.extend(c.drain(..));
            }
        });
    }
    pub fn release(&self, blocks: &[(usize, u32)]) {
        self.state.lock().unwrap().free.extend_from_slice(blocks);
    }
    /// Snapshot of block id -> address.
    pub fn block_ptrs(&self) -> Vec<usize> {
        self.state.lock().unwrap().ptrs.clone()
    }
}

/// Append-only bucket made of arena blocks.
pub struct Bucket<T: Copy> {
    cur: *mut T,
    end: *mut T,
    start: *mut T,
    cur_id: u32,
    blocks: Vec<(*mut T, usize, u32)>, // completed blocks (ptr, len, id)
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
            cur_id: 0,
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
    /// Push and return a handle (block id * per-block capacity + index).
    #[inline(always)]
    pub fn push_h(&mut self, v: T) -> u32 {
        if self.cur == self.end {
            self.grow();
        }
        let cap = arena().block_bytes() / std::mem::size_of::<T>();
        unsafe {
            let h = self.cur_id * cap as u32 + self.cur.offset_from(self.start) as u32;
            self.cur.write(v);
            self.cur = self.cur.add(1);
            h
        }
    }
    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        if !self.start.is_null() {
            let n = unsafe { self.cur.offset_from(self.start) as usize };
            self.blocks.push((self.start, n, self.cur_id));
            self.len += n;
        }
        let a = arena();
        let (p, id) = a.alloc();
        let p = p as *mut T;
        self.cur_id = id;
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
        for &(p, n, _) in &self.blocks {
            f(unsafe { std::slice::from_raw_parts(p, n) });
        }
        if !self.start.is_null() {
            let n = unsafe { self.cur.offset_from(self.start) as usize };
            f(unsafe { std::slice::from_raw_parts(self.start, n) });
        }
    }
    /// Return all blocks to the arena.
    pub fn release(&mut self) {
        let mut v: Vec<(usize, u32)> = self.blocks.iter().map(|&(p, _, id)| (p as usize, id)).collect();
        if !self.start.is_null() {
            v.push((self.start as usize, self.cur_id));
        }
        arena().release(&v);
        self.blocks.clear();
        self.start = std::ptr::null_mut();
        self.cur = std::ptr::null_mut();
        self.end = std::ptr::null_mut();
        self.len = 0;
    }
}

/// Zeroed, huge-page-backed array that lives for the rest of the process.
pub fn huge_zeroed<T: Copy>(n: usize) -> &'static mut [T] {
    let bytes = (n * std::mem::size_of::<T>()).max(4096);
    let p = map_region(bytes);
    unsafe { std::slice::from_raw_parts_mut(p as *mut T, n) }
}

/// A set of buckets with the hot write pointers packed densely (16 bytes per bucket),
/// so scattering into thousands of buckets keeps the headers cache-resident.
pub struct BucketSet<T: Copy> {
    hot: Vec<(*mut T, *mut T)>, // (cur, end)
    cold: Vec<Cold<T>>,
}
struct Cold<T> {
    start: *mut T,
    id: u32,
    blocks: Vec<(*mut T, usize, u32)>,
}
unsafe impl<T: Copy> Send for BucketSet<T> {}
unsafe impl<T: Copy> Sync for BucketSet<T> {}

impl<T: Copy> BucketSet<T> {
    pub fn new(n: usize) -> Self {
        BucketSet {
            hot: vec![(std::ptr::null_mut(), std::ptr::null_mut()); n],
            cold: (0..n).map(|_| Cold { start: std::ptr::null_mut(), id: 0, blocks: Vec::new() }).collect(),
        }
    }
    #[inline(always)]
    pub fn push(&mut self, b: usize, v: T) {
        let h = unsafe { self.hot.get_unchecked_mut(b) };
        if h.0 == h.1 {
            self.grow(b);
        }
        let h = unsafe { self.hot.get_unchecked_mut(b) };
        unsafe {
            h.0.write(v);
            h.0 = h.0.add(1);
        }
    }
    /// Push and return a handle (block id * per-block capacity + index).
    #[inline(always)]
    pub fn push_h(&mut self, b: usize, v: T) -> u32 {
        let h = unsafe { self.hot.get_unchecked_mut(b) };
        if h.0 == h.1 {
            self.grow(b);
        }
        let cap = (arena().block_bytes() / std::mem::size_of::<T>()) as u32;
        let c = unsafe { self.cold.get_unchecked(b) };
        let h = unsafe { self.hot.get_unchecked_mut(b) };
        unsafe {
            let handle = c.id * cap + h.0.offset_from(c.start) as u32;
            h.0.write(v);
            h.0 = h.0.add(1);
            handle
        }
    }
    #[cold]
    #[inline(never)]
    fn grow(&mut self, b: usize) {
        let c = &mut self.cold[b];
        let h = &mut self.hot[b];
        if !c.start.is_null() {
            let n = unsafe { h.0.offset_from(c.start) as usize };
            c.blocks.push((c.start, n, c.id));
        }
        let a = arena();
        let (p, id) = a.alloc();
        let p = p as *mut T;
        let cap = a.block_bytes() / std::mem::size_of::<T>();
        c.start = p;
        c.id = id;
        *h = (p, unsafe { p.add(cap) });
    }
    pub fn len(&self, b: usize) -> usize {
        let c = &self.cold[b];
        c.blocks.iter().map(|x| x.1).sum::<usize>()
            + if c.start.is_null() { 0 } else { unsafe { self.hot[b].0.offset_from(c.start) as usize } }
    }
    #[inline(always)]
    pub fn for_slices(&self, b: usize, mut f: impl FnMut(&[T])) {
        let c = &self.cold[b];
        for &(p, n, _) in &c.blocks {
            f(unsafe { std::slice::from_raw_parts(p, n) });
        }
        if !c.start.is_null() {
            let n = unsafe { self.hot[b].0.offset_from(c.start) as usize };
            f(unsafe { std::slice::from_raw_parts(c.start, n) });
        }
    }
    /// Return every block to the arena.
    pub fn release_all(&mut self) {
        let mut v: Vec<(usize, u32)> = Vec::new();
        for (b, c) in self.cold.iter_mut().enumerate() {
            for &(p, _, id) in &c.blocks {
                v.push((p as usize, id));
            }
            if !c.start.is_null() {
                v.push((c.start as usize, c.id));
            }
            c.blocks.clear();
            c.start = std::ptr::null_mut();
            self.hot[b] = (std::ptr::null_mut(), std::ptr::null_mut());
        }
        arena().release(&v);
    }
}

/// Bucket set with software write-combining: each bucket stages K elements (a whole number of
/// cache lines) in a small cache-resident buffer and flushes them with non-temporal stores,
/// avoiding read-for-ownership traffic on the scattered destination lines.
/// Call `finish()` (on the writing thread) before reading.
pub struct WcSet<T: Copy, const K: usize> {
    hot: Vec<(*mut T, *mut T)>, // (cur, end) in block memory
    cnt: Vec<u8>,
    buf: Vec<Stage<T, K>>,
    cold: Vec<Cold<T>>,
}
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Stage<T: Copy, const K: usize>([T; K]);
unsafe impl<T: Copy, const K: usize> Send for WcSet<T, K> {}
unsafe impl<T: Copy, const K: usize> Sync for WcSet<T, K> {}

impl<T: Copy, const K: usize> WcSet<T, K> {
    /// Elements per block (a multiple of K, so flush units never straddle blocks).
    pub fn cap() -> usize {
        arena().block_bytes() / std::mem::size_of::<T>() / K * K
    }
    pub fn new(n: usize) -> Self {
        assert!((K * std::mem::size_of::<T>()) % 64 == 0);
        WcSet {
            hot: vec![(std::ptr::null_mut(), std::ptr::null_mut()); n],
            cnt: vec![0; n],
            buf: vec![Stage(unsafe { std::mem::zeroed() }); n],
            cold: (0..n).map(|_| Cold { start: std::ptr::null_mut(), id: 0, blocks: Vec::new() }).collect(),
        }
    }
    #[cfg_attr(not(feature = "prof"), inline(always))]
    #[cfg_attr(feature = "prof", inline(never))]
    pub fn push(&mut self, b: usize, v: T) {
        unsafe {
            let c = *self.cnt.get_unchecked(b) as usize;
            *self.buf.get_unchecked_mut(b).0.get_unchecked_mut(c) = v;
            if c + 1 == K {
                self.flush(b);
                *self.cnt.get_unchecked_mut(b) = 0;
            } else {
                *self.cnt.get_unchecked_mut(b) = (c + 1) as u8;
            }
        }
    }
    /// Push and return the element's arena handle (block id * cap + index).
    #[cfg_attr(not(feature = "prof"), inline(always))]
    #[cfg_attr(feature = "prof", inline(never))]
    pub fn push_h(&mut self, b: usize, v: T) -> u32 {
        unsafe {
            let c = *self.cnt.get_unchecked(b) as usize;
            if c == 0 {
                let h = self.hot.get_unchecked(b);
                if h.0 == h.1 {
                    self.grow(b);
                }
            }
            let cold = self.cold.get_unchecked(b);
            let h = self.hot.get_unchecked(b);
            let handle = cold.id * Self::cap() as u32 + h.0.offset_from(cold.start) as u32 + c as u32;
            *self.buf.get_unchecked_mut(b).0.get_unchecked_mut(c) = v;
            if c + 1 == K {
                self.flush(b);
                *self.cnt.get_unchecked_mut(b) = 0;
            } else {
                *self.cnt.get_unchecked_mut(b) = (c + 1) as u8;
            }
            handle
        }
    }
    #[inline(always)]
    unsafe fn flush(&mut self, b: usize) {
        use std::arch::x86_64::*;
        let h = self.hot.get_unchecked(b);
        if h.0 == h.1 {
            self.grow(b);
        }
        let h = self.hot.get_unchecked_mut(b);
        let src = self.buf.get_unchecked(b).0.as_ptr() as *const u8;
        let dst = h.0 as *mut u8;
        let bytes = K * std::mem::size_of::<T>();
        let mut o = 0;
        while o < bytes {
            _mm256_stream_si256(dst.add(o) as *mut __m256i, _mm256_load_si256(src.add(o) as *const __m256i));
            o += 32;
        }
        h.0 = h.0.add(K);
    }
    #[cold]
    #[inline(never)]
    fn grow(&mut self, b: usize) {
        let c = &mut self.cold[b];
        let h = &mut self.hot[b];
        if !c.start.is_null() {
            let n = unsafe { h.0.offset_from(c.start) as usize };
            c.blocks.push((c.start, n, c.id));
        }
        let (p, id) = arena().alloc();
        let p = p as *mut T;
        c.start = p;
        c.id = id;
        *h = (p, unsafe { p.add(Self::cap()) });
    }
    /// Write out partially filled stage buffers and fence the non-temporal stores.
    pub fn finish(&mut self) {
        for b in 0..self.hot.len() {
            let n = self.cnt[b] as usize;
            if n == 0 {
                continue;
            }
            // a partial stage always fits: the block had room for a full K-unit at its first push
            let h = self.hot[b];
            if h.0 == h.1 {
                self.grow(b);
            }
            let h = &mut self.hot[b];
            unsafe {
                std::ptr::copy_nonoverlapping(self.buf[b].0.as_ptr(), h.0, n);
                h.0 = h.0.add(n);
            }
            self.cnt[b] = 0;
        }
        unsafe { std::arch::x86_64::_mm_sfence() };
    }
    pub fn len(&self, b: usize) -> usize {
        let c = &self.cold[b];
        c.blocks.iter().map(|x| x.1).sum::<usize>()
            + if c.start.is_null() { 0 } else { unsafe { self.hot[b].0.offset_from(c.start) as usize } }
    }
    #[inline(always)]
    pub fn for_slices(&self, b: usize, mut f: impl FnMut(&[T])) {
        let c = &self.cold[b];
        for &(p, n, _) in &c.blocks {
            f(unsafe { std::slice::from_raw_parts(p, n) });
        }
        if !c.start.is_null() {
            let n = unsafe { self.hot[b].0.offset_from(c.start) as usize };
            f(unsafe { std::slice::from_raw_parts(c.start, n) });
        }
    }
    pub fn release_all(&mut self) {
        let mut v: Vec<(usize, u32)> = Vec::new();
        for (b, c) in self.cold.iter_mut().enumerate() {
            for &(p, _, id) in &c.blocks {
                v.push((p as usize, id));
            }
            if !c.start.is_null() {
                v.push((c.start as usize, c.id));
            }
            c.blocks.clear();
            c.start = std::ptr::null_mut();
            self.hot[b] = (std::ptr::null_mut(), std::ptr::null_mut());
        }
        arena().release(&v);
    }
}

/// Return the calling thread's cached blocks to the shared pool (no-op before init).
#[allow(static_mut_refs)]
pub fn arena_flush_tl() {
    unsafe {
        if let Some(a) = ARENA.as_ref() {
            a.flush_thread_cache();
        }
    }
}
