//! Bounded JSON lines; discard oversized items through their newline to preserve framing.
use std::io::{self, BufRead};

pub(super) const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

pub(super) enum Line {
    Json(String),
    Dropped(usize),
}

pub(super) fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Line>> {
    read_bounded_line(reader, MAX_LINE_BYTES)
}

fn read_bounded_line(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Line>> {
    let mut bytes = Vec::new();
    let mut size = 0usize;
    loop {
        let buf = match reader.fill_buf() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if buf.is_empty() {
            break;
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        let end = buf[n - 1] == b'\n';
        size = size.saturating_add(n);
        if size <= limit {
            bytes.extend_from_slice(&buf[..n]);
        } else {
            bytes = Vec::new();
        }
        reader.consume(n);
        if end {
            break;
        }
    }
    if size == 0 {
        return Ok(None);
    }
    if size > limit {
        eprintln!("protocol: dropped oversized line ({size} bytes; limit {limit} bytes)");
        return Ok(Some(Line::Dropped(size)));
    }
    String::from_utf8(bytes)
        .map(|line| Some(Line::Json(line)))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn bounded_lines_preserve_utf8_framing_and_eof() {
        let mut reader = BufReader::with_capacity(2, Cursor::new("é\n1234\nok\nend"));
        assert!(
            matches!(read_bounded_line(&mut reader, 3).unwrap(), Some(Line::Json(s)) if s == "é\n")
        );
        assert!(matches!(
            read_bounded_line(&mut reader, 3).unwrap(),
            Some(Line::Dropped(5))
        ));
        assert!(
            matches!(read_bounded_line(&mut reader, 3).unwrap(), Some(Line::Json(s)) if s == "ok\n")
        );
        assert!(
            matches!(read_bounded_line(&mut reader, 3).unwrap(), Some(Line::Json(s)) if s == "end")
        );
        assert!(read_bounded_line(&mut reader, 3).unwrap().is_none());
        let mut reader = Cursor::new(b"1234");
        assert!(matches!(
            read_bounded_line(&mut reader, 3).unwrap(),
            Some(Line::Dropped(4))
        ));
        assert!(read_bounded_line(&mut reader, 3).unwrap().is_none());
        assert_eq!(
            read_bounded_line(&mut Cursor::new(b"\xff\n"), 3)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
