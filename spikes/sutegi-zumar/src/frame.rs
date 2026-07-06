//! Inbound (client→server) live-transport frames. Server mirror of the
//! encoder in www/zumar-live.js — keep in lockstep:
//!
//!   frame    = ver:u8=1 kind:u8 body
//!   dispatch = kind 0  n:varint path*n:varint  name:str  flags:u8
//!              [value:str] [key:str]
//!              flags: bit0 value present · bit1 checked present
//!                     bit2 checked value · bit3 key present
//!   resolve  = kind 1  id:varint ok:u8 status:varint body:str
//!   notify   = kind 2  id:varint now:varint (ms since epoch)
//!   str      = len:varint utf8
//!
//! Every read is bounds-checked: a malformed frame is an `Err`, never a
//! panic — this parser faces the network.

const VERSION: u8 = 1;
const MAX_PATH: u64 = 64;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Dispatch {
        path: Vec<u32>,
        name: String,
        value: Option<String>,
        checked: Option<bool>,
        key: Option<String>,
    },
    Resolve {
        id: u32,
        ok: bool,
        status: u16,
        body: String,
    },
    Notify {
        id: u32,
        now: f64,
    },
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.i).ok_or("truncated frame")?;
        self.i += 1;
        Ok(v)
    }

    fn vu(&mut self) -> Result<u64, String> {
        let mut n: u64 = 0;
        let mut shift = 0u32;
        for _ in 0..10 {
            let byte = self.u8()?;
            n |= u64::from(byte & 0x7f)
                .checked_shl(shift)
                .ok_or("varint overflow")?;
            if byte & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
        Err("varint too long".into())
    }

    fn str(&mut self) -> Result<String, String> {
        let len = self.vu()? as usize;
        let end = self.i.checked_add(len).ok_or("truncated string")?;
        if end > self.b.len() {
            return Err("truncated string".into());
        }
        let s = std::str::from_utf8(&self.b[self.i..end]).map_err(|_| "invalid utf-8")?;
        self.i = end;
        Ok(s.to_string())
    }
}

pub fn decode(bytes: &[u8]) -> Result<Frame, String> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.u8()? != VERSION {
        return Err("unknown frame version".into());
    }
    match r.u8()? {
        0 => {
            let depth = r.vu()?;
            if depth > MAX_PATH {
                return Err("path too deep".into());
            }
            let mut path = Vec::with_capacity(depth as usize);
            for _ in 0..depth {
                path.push(u32::try_from(r.vu()?).map_err(|_| "path index overflow")?);
            }
            let name = r.str()?;
            let flags = r.u8()?;
            let value = if flags & 1 != 0 { Some(r.str()?) } else { None };
            let checked = if flags & 2 != 0 {
                Some(flags & 4 != 0)
            } else {
                None
            };
            let key = if flags & 8 != 0 { Some(r.str()?) } else { None };
            Ok(Frame::Dispatch {
                path,
                name,
                value,
                checked,
                key,
            })
        }
        1 => {
            let id = u32::try_from(r.vu()?).map_err(|_| "id overflow")?;
            let ok = r.u8()? != 0;
            let status = u16::try_from(r.vu()?).map_err(|_| "status overflow")?;
            let body = r.str()?;
            Ok(Frame::Resolve {
                id,
                ok,
                status,
                body,
            })
        }
        2 => {
            let id = u32::try_from(r.vu()?).map_err(|_| "id overflow")?;
            let now = r.vu()? as f64;
            Ok(Frame::Notify { id, now })
        }
        _ => Err("unknown frame kind".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vu(buf: &mut Vec<u8>, mut n: u64) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                buf.push(b);
                break;
            }
            buf.push(b | 0x80);
        }
    }

    fn s(buf: &mut Vec<u8>, text: &str) {
        vu(buf, text.len() as u64);
        buf.extend_from_slice(text.as_bytes());
    }

    #[test]
    fn dispatch_click_no_payload() {
        let mut b = vec![1u8, 0];
        vu(&mut b, 2);
        vu(&mut b, 2);
        vu(&mut b, 2);
        s(&mut b, "click");
        b.push(0);
        assert_eq!(
            decode(&b).unwrap(),
            Frame::Dispatch {
                path: vec![2, 2],
                name: "click".into(),
                value: None,
                checked: None,
                key: None,
            }
        );
    }

    #[test]
    fn dispatch_with_value_and_checked() {
        let mut b = vec![1u8, 0];
        vu(&mut b, 0);
        s(&mut b, "input");
        b.push(1 | 2 | 4);
        s(&mut b, "eñe");
        let f = decode(&b).unwrap();
        assert_eq!(
            f,
            Frame::Dispatch {
                path: vec![],
                name: "input".into(),
                value: Some("eñe".into()),
                checked: Some(true),
                key: None,
            }
        );
    }

    #[test]
    fn resolve_and_notify() {
        let mut b = vec![1u8, 1];
        vu(&mut b, 7);
        b.push(1);
        vu(&mut b, 200);
        s(&mut b, "body");
        assert_eq!(
            decode(&b).unwrap(),
            Frame::Resolve {
                id: 7,
                ok: true,
                status: 200,
                body: "body".into()
            }
        );

        let mut b = vec![1u8, 2];
        vu(&mut b, 3);
        vu(&mut b, 1_720_000_000_000);
        assert_eq!(
            decode(&b).unwrap(),
            Frame::Notify {
                id: 3,
                now: 1_720_000_000_000.0
            }
        );
    }

    #[test]
    fn malformed_frames_error_not_panic() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[9, 0]).is_err()); // bad version
        assert!(decode(&[1, 9]).is_err()); // bad kind
        assert!(decode(&[1, 0, 0xff]).is_err()); // truncated varint
        let mut b = vec![1u8, 0];
        vu(&mut b, 65); // path deeper than MAX_PATH
        assert!(decode(&b).is_err());
        let mut b = vec![1u8, 0, 0];
        s(&mut b, "click");
        b.push(1); // claims a value, then nothing
        assert!(decode(&b).is_err());
        // truncated + garbage suffixes of a valid frame
        let mut good = vec![1u8, 0];
        vu(&mut good, 1);
        vu(&mut good, 0);
        s(&mut good, "click");
        good.push(0);
        for i in 0..good.len() {
            assert!(decode(&good[..i]).is_err(), "prefix {i} should fail");
        }
    }
}
