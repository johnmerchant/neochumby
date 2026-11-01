//! cpio's newc format, as the kernel's initramfs unpacker reads it: a 110
//! byte header of hexadecimal fields, the name with its NUL, padding to 4
//! bytes, the data, padding to 4 bytes; `TRAILER!!!` ends the archive.

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

/// Zeros that take `n` to a 4-byte boundary.
pub fn padding(n: usize) -> usize {
    n.wrapping_neg() & 3
}

pub struct Writer {
    out: Vec<u8>,
    ino: u32,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { out: Vec::new(), ino: 1 }
    }

    pub fn dir(&mut self, name: &str, mode: u32) {
        self.entry(name, S_IFDIR | mode, 2, &[]);
    }

    pub fn file(&mut self, name: &str, mode: u32, data: &[u8]) {
        self.entry(name, S_IFREG | mode, 1, data);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.entry("TRAILER!!!", 0, 1, &[]);
        self.out
    }

    fn entry(&mut self, name: &str, mode: u32, nlink: u32, data: &[u8]) {
        let ino = self.ino;
        self.ino += 1;
        let namesize = name.len() + 1;
        let fields = [ino, mode, 0, 0, nlink, 0, data.len() as u32, 0, 0, 0, 0, namesize as u32, 0];
        self.out.extend_from_slice(b"070701");
        for f in fields {
            self.out.extend_from_slice(format!("{f:08x}").as_bytes());
        }
        self.out.extend_from_slice(name.as_bytes());
        self.out.push(0);
        self.pad();
        self.out.extend_from_slice(data);
        self.pad();
    }

    fn pad(&mut self) {
        let n = padding(self.out.len());
        self.out.extend_from_slice(&[0u8; 3][..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_reaches_four_bytes() {
        assert_eq!([padding(0), padding(1), padding(2), padding(3), padding(4)], [0, 3, 2, 1, 0]);
    }

    #[test]
    fn a_file_is_laid_out_as_newc() {
        let mut w = Writer::new();
        w.file("psp/a", 0o600, b"hello");
        let out = w.finish();
        assert_eq!(&out[..6], b"070701");
        // mode, then filesize and namesize, in their places.
        assert_eq!(&out[14..22], b"00008180");
        assert_eq!(&out[54..62], b"00000005");
        assert_eq!(&out[94..102], b"00000006");
        // 110 + "psp/a\0" = 116, on the boundary, so the data follows.
        assert_eq!(&out[110..116], b"psp/a\0");
        assert_eq!(&out[116..121], b"hello");
        assert_eq!(&out[121..124], &[0, 0, 0]);
        assert_eq!(&out[124..130], b"070701");
        assert!(out.windows(10).any(|w| w == b"TRAILER!!!"));
        assert_eq!(out.len() % 4, 0);
    }

    #[test]
    fn host_cpio_reads_it_if_there_is_one() {
        // The layout's real reader is the kernel; a host cpio is a second
        // opinion where it exists.
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut w = Writer::new();
        w.dir("psp", 0o755);
        w.file("psp/network_config", 0o644, b"<configuration />");
        let Ok(mut c) = Command::new("cpio")
            .args(["-it", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        c.stdin.take().unwrap().write_all(&w.finish()).unwrap();
        let out = c.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "psp\npsp/network_config\n");
    }
}
