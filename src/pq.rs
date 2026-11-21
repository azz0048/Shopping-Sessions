//! Minimal Parquet writer: one row group; per column either one PLAIN data page or a
//! dictionary page + one RLE_DICTIONARY data page (for low-cardinality columns). Uncompressed.
//! Supports required INT32 (optionally DATE), INT64, BYTE_ARRAY (UTF8) and optional BYTE_ARRAY.
use std::collections::HashMap;
use std::io::Write;

pub enum Data<'a> {
    I32(&'a [i32]),
    I64(&'a [i64]),
    Str(&'a [&'a [u8]]),
    /// Optional strings: one Option per row.
    OptStr(&'a [Option<&'a [u8]>]),
}

pub struct Column<'a> {
    pub name: &'a str,
    pub data: Data<'a>,
    pub date: bool, // INT32 annotated as DATE
}

// ---- thrift compact protocol ----
struct T {
    b: Vec<u8>,
    last: Vec<i16>,
}
const CT_I32: u8 = 5;
const CT_I64: u8 = 6;
const CT_BIN: u8 = 8;
const CT_LIST: u8 = 9;
const CT_STRUCT: u8 = 12;

impl T {
    fn new() -> T {
        T { b: Vec::new(), last: vec![0] }
    }
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.b.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.b.push(v as u8);
    }
    fn zz(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64);
    }
    fn field(&mut self, id: i16, ty: u8) {
        let last = *self.last.last().unwrap();
        let d = id - last;
        if d > 0 && d <= 15 {
            self.b.push(((d as u8) << 4) | ty);
        } else {
            self.b.push(ty);
            let z = ((id as i64) << 1) ^ ((id as i64) >> 63);
            self.varint(z as u64);
        }
        *self.last.last_mut().unwrap() = id;
    }
    fn i32f(&mut self, id: i16, v: i32) {
        self.field(id, CT_I32);
        self.zz(v as i64);
    }
    fn i64f(&mut self, id: i16, v: i64) {
        self.field(id, CT_I64);
        self.zz(v);
    }
    fn strf(&mut self, id: i16, s: &[u8]) {
        self.field(id, CT_BIN);
        self.varint(s.len() as u64);
        self.b.extend_from_slice(s);
    }
    fn begin_struct(&mut self, id: i16) {
        self.field(id, CT_STRUCT);
        self.last.push(0);
    }
    fn begin_elem(&mut self) {
        self.last.push(0);
    }
    fn end_struct(&mut self) {
        self.b.push(0);
        self.last.pop();
    }
    fn list_header(&mut self, id: i16, elem: u8, n: usize) {
        self.field(id, CT_LIST);
        if n < 15 {
            self.b.push(((n as u8) << 4) | elem);
        } else {
            self.b.push(0xF0 | elem);
            self.varint(n as u64);
        }
    }
}

fn phys(d: &Data) -> i32 {
    match d {
        Data::I32(_) => 1,
        Data::I64(_) => 2,
        _ => 6,
    }
}

const PLAIN: i32 = 0;
const RLE: i32 = 3;
const RLE_DICT: i32 = 8;

fn def_levels(v: &[Option<&[u8]>], out: &mut Vec<u8>) {
    let n = v.len();
    let groups = n.div_ceil(8);
    let mut hdr = T::new();
    hdr.varint(((groups as u64) << 1) | 1);
    out.extend_from_slice(&((hdr.b.len() + groups) as u32).to_le_bytes());
    out.extend_from_slice(&hdr.b);
    let start = out.len();
    out.resize(start + groups, 0);
    for (i, s) in v.iter().enumerate() {
        if s.is_some() {
            out[start + i / 8] |= 1 << (i % 8);
        }
    }
}

/// Bit width byte + one bit-packed run of the indices (LSB-first).
fn dict_indices(idx: &[u32], ndict: usize, out: &mut Vec<u8>) {
    let bw = (usize::BITS - (ndict.max(2) - 1).leading_zeros()) as usize;
    out.push(bw as u8);
    let groups = idx.len().div_ceil(8);
    let mut hdr = T::new();
    hdr.varint(((groups as u64) << 1) | 1);
    out.extend_from_slice(&hdr.b);
    let start = out.len();
    out.resize(start + groups * bw + 8, 0);
    let buf = &mut out[start..];
    let mut bit = 0usize;
    for &v in idx {
        // write up to 32 bits at bit offset `bit` with an unaligned 8-byte read-modify-write
        let byte = bit / 8;
        let off = bit % 8;
        let mut w = u64::from_le_bytes(buf[byte..byte + 8].try_into().unwrap());
        w |= (v as u64) << off;
        buf[byte..byte + 8].copy_from_slice(&w.to_le_bytes());
        bit += bw;
    }
    out.truncate(start + groups * bw);
}

struct Pages {
    dict: Option<(Vec<u8>, usize)>, // body, number of dictionary values
    data: Vec<u8>,
    encoding: i32,
}

fn plain_i32(v: &[i32], data: &mut Vec<u8>) {
    data.reserve(v.len() * 4);
    for x in v.iter() {
        data.extend_from_slice(&x.to_le_bytes());
    }
}

fn encode(d: &Data) -> Pages {
    let mut data = Vec::new();
    match d {
        Data::I32(v) => {
            let (mn, mx) = v.iter().fold((i32::MAX, i32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            let range = if v.is_empty() { 0 } else { (mx as i64 - mn as i64 + 1) as usize };
            if v.is_empty() || range > 1 << 20 {
                plain_i32(v, &mut data);
                return Pages { dict: None, data, encoding: PLAIN };
            }
            let mut map = vec![u32::MAX; range];
            let mut dict = Vec::new();
            let mut idx = Vec::with_capacity(v.len());
            for &x in v.iter() {
                let slot = &mut map[(x - mn) as usize];
                if *slot == u32::MAX {
                    *slot = dict.len() as u32;
                    dict.push(x);
                }
                idx.push(*slot);
            }
            if dict.len() > 1 << 16 {
                plain_i32(v, &mut data);
                return Pages { dict: None, data, encoding: PLAIN };
            }
            let mut db = Vec::with_capacity(dict.len() * 4);
            for x in &dict {
                db.extend_from_slice(&x.to_le_bytes());
            }
            dict_indices(&idx, dict.len(), &mut data);
            Pages { dict: Some((db, dict.len())), data, encoding: RLE_DICT }
        }
        Data::I64(v) => {
            data.reserve(v.len() * 8);
            for x in v.iter() {
                data.extend_from_slice(&x.to_le_bytes());
            }
            Pages { dict: None, data, encoding: PLAIN }
        }
        Data::Str(v) => {
            let total: usize = v.iter().map(|s| s.len() + 4).sum();
            data.reserve(total);
            for s in v.iter() {
                data.extend_from_slice(&(s.len() as u32).to_le_bytes());
                data.extend_from_slice(s);
            }
            Pages { dict: None, data, encoding: PLAIN }
        }
        Data::OptStr(v) => {
            def_levels(v, &mut data);
            let mut map: HashMap<&[u8], u32> = HashMap::new();
            let mut dict: Vec<&[u8]> = Vec::new();
            let mut idx = Vec::new();
            for s in v.iter().flatten() {
                let n = dict.len() as u32;
                let id = *map.entry(s).or_insert(n);
                if id == n {
                    dict.push(s);
                }
                idx.push(id);
            }
            let mut db = Vec::new();
            for s in &dict {
                db.extend_from_slice(&(s.len() as u32).to_le_bytes());
                db.extend_from_slice(s);
            }
            dict_indices(&idx, dict.len(), &mut data);
            Pages { dict: Some((db, dict.len())), data, encoding: RLE_DICT }
        }
    }
}

fn page_header(kind: i32, body_len: usize, nvals: usize, encoding: i32) -> Vec<u8> {
    let mut h = T::new();
    h.i32f(1, kind);
    h.i32f(2, body_len as i32);
    h.i32f(3, body_len as i32);
    if kind == 0 {
        h.begin_struct(5);
        h.i32f(1, nvals as i32);
        h.i32f(2, encoding);
        h.i32f(3, RLE);
        h.i32f(4, RLE);
        h.end_struct();
    } else {
        h.begin_struct(7);
        h.i32f(1, nvals as i32);
        h.i32f(2, PLAIN);
        h.end_struct();
    }
    h.b.push(0);
    h.b
}

pub fn write(path: &std::path::Path, cols: &[Column], nrows: usize) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    f.write_all(b"PAR1")?;
    let mut pos = 4i64;
    struct Meta {
        dict_off: Option<i64>,
        data_off: i64,
        size: i64,
        encoding: i32,
    }
    let mut metas = Vec::new();
    let (mut tenc, mut twr) = (0f64, 0f64);
    for c in cols {
        let t0 = std::time::Instant::now();
        let pages = encode(&c.data);
        tenc += t0.elapsed().as_secs_f64();
        let t1 = std::time::Instant::now();
        let start = pos;
        let mut dict_off = None;
        if let Some((db, nd)) = &pages.dict {
            let h = page_header(2, db.len(), *nd, PLAIN);
            dict_off = Some(pos);
            f.write_all(&h)?;
            f.write_all(db)?;
            pos += (h.len() + db.len()) as i64;
        }
        let h = page_header(0, pages.data.len(), nrows, pages.encoding);
        let data_off = pos;
        f.write_all(&h)?;
        f.write_all(&pages.data)?;
        pos += (h.len() + pages.data.len()) as i64;
        metas.push(Meta { dict_off, data_off, size: pos - start, encoding: pages.encoding });
        twr += t1.elapsed().as_secs_f64();
    }
    // FileMetaData
    let mut m = T::new();
    m.i32f(1, 1);
    m.list_header(2, CT_STRUCT, cols.len() + 1);
    m.begin_elem();
    m.strf(4, b"schema");
    m.i32f(5, cols.len() as i32);
    m.end_struct();
    for c in cols {
        m.begin_elem();
        m.i32f(1, phys(&c.data));
        let optional = matches!(c.data, Data::OptStr(_));
        m.i32f(3, if optional { 1 } else { 0 });
        m.strf(4, c.name.as_bytes());
        let is_str = matches!(c.data, Data::Str(_) | Data::OptStr(_));
        if is_str {
            m.i32f(6, 0); // UTF8
            m.begin_struct(10);
            m.begin_struct(1); // StringType
            m.end_struct();
            m.end_struct();
        } else if c.date {
            m.i32f(6, 6); // DATE
            m.begin_struct(10);
            m.begin_struct(6); // DateType
            m.end_struct();
            m.end_struct();
        }
        m.end_struct();
    }
    m.i64f(3, nrows as i64);
    m.list_header(4, CT_STRUCT, 1);
    m.begin_elem();
    m.list_header(1, CT_STRUCT, cols.len());
    let mut total = 0i64;
    for (c, mt) in cols.iter().zip(&metas) {
        total += mt.size;
        m.begin_elem();
        m.i64f(2, mt.dict_off.unwrap_or(mt.data_off));
        m.begin_struct(3);
        m.i32f(1, phys(&c.data));
        let encs: &[i32] = if mt.encoding == RLE_DICT { &[PLAIN, RLE, RLE_DICT] } else { &[PLAIN, RLE] };
        m.list_header(2, CT_I32, encs.len());
        for &e in encs {
            m.zz(e as i64);
        }
        m.list_header(3, CT_BIN, 1);
        m.varint(c.name.len() as u64);
        m.b.extend_from_slice(c.name.as_bytes());
        m.i32f(4, 0); // UNCOMPRESSED
        m.i64f(5, nrows as i64);
        m.i64f(6, mt.size);
        m.i64f(7, mt.size);
        m.i64f(9, mt.data_off);
        if let Some(d) = mt.dict_off {
            m.i64f(11, d);
        }
        m.end_struct();
        m.end_struct();
    }
    m.i64f(2, total);
    m.i64f(3, nrows as i64);
    m.end_struct();
    m.strf(6, b"shopping-sessions");
    m.b.push(0);
    f.write_all(&m.b)?;
    f.write_all(&(m.b.len() as u32).to_le_bytes())?;
    f.write_all(b"PAR1")?;
    let t2 = std::time::Instant::now();
    f.flush()?;
    drop(f);
    if std::env::var_os("SS_VERBOSE").is_some() {
        eprintln!("          pq {}: encode {:.0} ms, write {:.0} ms, flush+close {:.0} ms", nrows, tenc * 1e3, twr * 1e3, t2.elapsed().as_secs_f64() * 1e3);
    }
    Ok(())
}
