//! Minimal Parquet writer: one row group, one uncompressed PLAIN data page (v1) per column.
//! Supports required INT32 (optionally DATE), INT64, BYTE_ARRAY (UTF8) and optional BYTE_ARRAY.
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
    /// Start a struct that is a list element (no field header).
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

/// Encode one column's page body (def levels + values).
fn page_body(d: &Data, out: &mut Vec<u8>) -> usize {
    match d {
        Data::I32(v) => {
            out.reserve(v.len() * 4);
            for x in v.iter() {
                out.extend_from_slice(&x.to_le_bytes());
            }
            v.len()
        }
        Data::I64(v) => {
            out.reserve(v.len() * 8);
            for x in v.iter() {
                out.extend_from_slice(&x.to_le_bytes());
            }
            v.len()
        }
        Data::Str(v) => {
            let total: usize = v.iter().map(|s| s.len() + 4).sum();
            out.reserve(total);
            for s in v.iter() {
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s);
            }
            v.len()
        }
        Data::OptStr(v) => {
            // definition levels: RLE/bit-packed hybrid, one bit-packed run, 4-byte length prefix
            let n = v.len();
            let groups = n.div_ceil(8);
            let mut hdr = T::new();
            hdr.varint(((groups as u64) << 1) | 1);
            let lev_len = hdr.b.len() + groups;
            out.extend_from_slice(&(lev_len as u32).to_le_bytes());
            out.extend_from_slice(&hdr.b);
            let start = out.len();
            out.resize(start + groups, 0);
            for (i, s) in v.iter().enumerate() {
                if s.is_some() {
                    out[start + i / 8] |= 1 << (i % 8);
                }
            }
            for s in v.iter().flatten() {
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s);
            }
            n
        }
    }
}

pub fn write(path: &std::path::Path, cols: &[Column], nrows: usize) -> std::io::Result<()> {
    let mut file: Vec<u8> = Vec::with_capacity(1 << 20);
    file.extend_from_slice(b"PAR1");
    struct Meta {
        offset: i64,
        size: i64,
        nvals: i64,
    }
    let mut metas = Vec::new();
    let mut body = Vec::new();
    for c in cols {
        body.clear();
        let nvals = page_body(&c.data, &mut body);
        assert_eq!(nvals, nrows);
        let mut h = T::new();
        h.i32f(1, 0); // DATA_PAGE
        h.i32f(2, body.len() as i32);
        h.i32f(3, body.len() as i32);
        h.begin_struct(5);
        h.i32f(1, nvals as i32);
        h.i32f(2, 0); // PLAIN
        h.i32f(3, 3); // RLE
        h.i32f(4, 3); // RLE
        h.end_struct();
        h.b.push(0); // end PageHeader
        let offset = file.len() as i64;
        file.extend_from_slice(&h.b);
        file.extend_from_slice(&body);
        metas.push(Meta { offset, size: (h.b.len() + body.len()) as i64, nvals: nvals as i64 });
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
        m.i64f(2, mt.offset);
        m.begin_struct(3);
        m.i32f(1, phys(&c.data));
        let optional = matches!(c.data, Data::OptStr(_));
        let encs: &[i32] = if optional { &[0, 3] } else { &[0] };
        m.list_header(2, CT_I32, encs.len());
        for &e in encs {
            m.zz(e as i64);
        }
        m.list_header(3, CT_BIN, 1);
        m.varint(c.name.len() as u64);
        m.b.extend_from_slice(c.name.as_bytes());
        m.i32f(4, 0); // UNCOMPRESSED
        m.i64f(5, mt.nvals);
        m.i64f(6, mt.size);
        m.i64f(7, mt.size);
        m.i64f(9, mt.offset);
        m.end_struct();
        m.end_struct();
    }
    m.i64f(2, total);
    m.i64f(3, nrows as i64);
    m.end_struct();
    m.strf(6, b"shopping-sessions");
    m.b.push(0);
    file.extend_from_slice(&m.b);
    file.extend_from_slice(&(m.b.len() as u32).to_le_bytes());
    file.extend_from_slice(b"PAR1");
    let mut f = std::fs::File::create(path)?;
    f.write_all(&file)?;
    Ok(())
}
