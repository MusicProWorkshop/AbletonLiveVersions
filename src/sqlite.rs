//! A minimal, read-only SQLite reader: enough to list the rows of one table.
//! Used for Live's plug-in database, so the project needs no C compiler.
//! Follows https://www.sqlite.org/fileformat.html, including the write-ahead log
//! (Live keeps the database open, so recent changes are often still in "-wal").
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

pub struct Db {
    data: Vec<u8>,
    page_size: usize,
    usable: usize,
    wal: HashMap<u32, Vec<u8>>,
}

fn be16(b: &[u8], o: usize) -> usize {
    u16::from_be_bytes([b[o], b[o + 1]]) as usize
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// SQLite varint: 1–9 bytes. Returns (value, bytes used).
fn varint(b: &[u8], o: usize) -> Result<(i64, usize), String> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b.get(o + i).ok_or("varint past end of page")?;
        if i == 8 {
            return Ok((((v << 8) | byte as u64) as i64, 9));
        }
        v = (v << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Ok((v as i64, i + 1));
        }
    }
    unreachable!()
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

impl Db {
    pub fn open(path: &Path) -> std::io::Result<Db> {
        let data = std::fs::read(path)?;
        if data.len() < 100 || &data[..16] != b"SQLite format 3\0" {
            return Err(bad("not an SQLite database"));
        }
        let ps = match be16(&data, 16) {
            1 => 65536,
            n => n,
        };
        let usable = ps - data[20] as usize;
        let mut db = Db { data, page_size: ps, usable, wal: HashMap::new() };
        let wal_path = format!("{}-wal", path.display());
        if let Ok(w) = std::fs::read(wal_path) {
            db.load_wal(&w);
        }
        Ok(db)
    }

    /// Pages from committed transactions in the write-ahead log override the main file.
    fn load_wal(&mut self, w: &[u8]) {
        if w.len() < 32 || !matches!(be32(w, 0), 0x377f0682 | 0x377f0683) || be32(w, 8) as usize != self.page_size {
            return;
        }
        let (salt1, salt2) = (be32(w, 16), be32(w, 20));
        let frame = 24 + self.page_size;
        let mut pending: Vec<(u32, usize)> = Vec::new();
        let mut off = 32;
        while off + frame <= w.len() {
            if be32(w, off + 8) != salt1 || be32(w, off + 12) != salt2 {
                break;
            }
            pending.push((be32(w, off), off + 24));
            if be32(w, off + 4) != 0 {
                // commit frame: everything up to here is part of the database
                for (pg, at) in pending.drain(..) {
                    self.wal.insert(pg, w[at..at + self.page_size].to_vec());
                }
            }
            off += frame;
        }
    }

    fn page(&self, n: u32) -> Result<&[u8], String> {
        if let Some(p) = self.wal.get(&n) {
            return Ok(p);
        }
        let start = (n as usize - 1) * self.page_size;
        self.data.get(start..start + self.page_size).ok_or_else(|| format!("page {n} out of range"))
    }

    /// Payload of a cell whose total size is `total`, starting at `at` on `page`.
    fn payload(&self, page: &[u8], at: usize, total: usize) -> Result<Vec<u8>, String> {
        let u = self.usable;
        let x = u - 35;
        let local = if total <= x {
            total
        } else {
            let m = ((u - 12) * 32 / 255) - 23;
            let k = m + ((total - m) % (u - 4));
            if k <= x { k } else { m }
        };
        let mut out = page.get(at..at + local).ok_or("payload past end of page")?.to_vec();
        if local < total {
            let mut next = be32(page, at + local);
            while out.len() < total && next != 0 {
                let p = self.page(next)?;
                let take = (total - out.len()).min(u - 4);
                out.extend_from_slice(&p[4..4 + take]);
                next = be32(p, 0);
            }
        }
        Ok(out)
    }

    /// All records of the table b-tree rooted at `root`, as (rowid, payload).
    fn walk(&self, root: u32, out: &mut Vec<(i64, Vec<u8>)>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("b-tree too deep".into());
        }
        let page = self.page(root)?;
        let h = if root == 1 { 100 } else { 0 };
        let kind = page[h];
        let cells = be16(page, h + 3);
        match kind {
            0x0D => {
                for i in 0..cells {
                    let mut at = be16(page, h + 8 + i * 2);
                    let (size, n) = varint(page, at)?;
                    at += n;
                    let (rowid, n) = varint(page, at)?;
                    at += n;
                    out.push((rowid, self.payload(page, at, size as usize)?));
                }
            }
            0x05 => {
                for i in 0..cells {
                    let at = be16(page, h + 12 + i * 2);
                    self.walk(be32(page, at), out, depth + 1)?;
                }
                self.walk(be32(page, h + 8), out, depth + 1)?;
            }
            k => return Err(format!("unexpected page type {k:#x}")),
        }
        Ok(())
    }

    fn record(p: &[u8]) -> Result<Vec<Value>, String> {
        let (hsize, mut o) = varint(p, 0)?;
        let mut types = Vec::new();
        while o < hsize as usize {
            let (t, n) = varint(p, o)?;
            types.push(t);
            o += n;
        }
        let mut at = hsize as usize;
        let mut vals = Vec::with_capacity(types.len());
        for t in types {
            let int = |len: usize, at: usize| -> Result<i64, String> {
                let b = p.get(at..at + len).ok_or("record past end")?;
                let mut v: i64 = if b[0] & 0x80 != 0 { -1 } else { 0 };
                for x in b {
                    v = (v << 8) | *x as i64;
                }
                Ok(v)
            };
            let (v, len) = match t {
                0 => (Value::Null, 0),
                1 => (Value::Int(int(1, at)?), 1),
                2 => (Value::Int(int(2, at)?), 2),
                3 => (Value::Int(int(3, at)?), 3),
                4 => (Value::Int(int(4, at)?), 4),
                5 => (Value::Int(int(6, at)?), 6),
                6 => (Value::Int(int(8, at)?), 8),
                7 => (Value::Float(f64::from_bits(int(8, at)? as u64)), 8),
                8 => (Value::Int(0), 0),
                9 => (Value::Int(1), 0),
                n if n >= 12 && n % 2 == 0 => {
                    let len = ((n - 12) / 2) as usize;
                    (Value::Blob(p.get(at..at + len).ok_or("blob past end")?.to_vec()), len)
                }
                n if n >= 13 => {
                    let len = ((n - 13) / 2) as usize;
                    (Value::Text(String::from_utf8_lossy(p.get(at..at + len).ok_or("text past end")?).into_owned()), len)
                }
                _ => (Value::Null, 0),
            };
            vals.push(v);
            at += len;
        }
        Ok(vals)
    }

    /// Rows of a table as {column name: value}. INTEGER PRIMARY KEY columns get the rowid.
    pub fn rows(&self, table: &str) -> Result<Vec<HashMap<String, Value>>, String> {
        let mut master = Vec::new();
        self.walk(1, &mut master, 0)?;
        let mut root = None;
        let mut sql = String::new();
        for (_, p) in &master {
            let r = Self::record(p)?;
            if r.first().and_then(Value::text) == Some("table") && r.get(1).and_then(Value::text) == Some(table) {
                if let Some(Value::Int(n)) = r.get(3) {
                    root = Some(*n as u32);
                }
                sql = r.get(4).and_then(Value::text).unwrap_or("").to_string();
            }
        }
        let root = root.ok_or_else(|| format!("table {table} not found"))?;
        let (open, close) = (sql.find('(').ok_or("bad schema")?, sql.rfind(')').ok_or("bad schema")?);
        let cols: Vec<(String, bool)> = sql[open + 1..close]
            .split(',')
            .map(|c| {
                let c = c.trim();
                let name = c.split_whitespace().next().unwrap_or("").trim_matches(|ch| ch == '"' || ch == '`' || ch == '[' || ch == ']');
                (name.to_string(), c.to_lowercase().contains("integer primary key"))
            })
            .collect();
        let mut raw = Vec::new();
        self.walk(root, &mut raw, 0)?;
        let mut out = Vec::with_capacity(raw.len());
        for (rowid, p) in raw {
            let vals = Self::record(&p)?;
            let mut row = HashMap::new();
            for (i, (name, is_pk)) in cols.iter().enumerate() {
                let v = vals.get(i).cloned().unwrap_or(Value::Null);
                let v = if *is_pk && v == Value::Null { Value::Int(rowid) } else { v };
                row.insert(name.clone(), v);
            }
            out.push(row);
        }
        Ok(out)
    }
}
