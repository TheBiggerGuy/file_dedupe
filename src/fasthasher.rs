use std::result::Result::Ok;
use std::io::{self, Read};
use std::path::PathBuf;
use std::fs::OpenOptions;
use std::cell::RefCell;

use crc::crc32::{self, Hasher32};

struct FastHasher {
    digest: crc32::Digest,
    buffer: Vec<u8>,
}

impl FastHasher {
    fn new(buffer_size: usize) -> FastHasher {
        FastHasher {
            digest: crc32::Digest::new(crc32::IEEE),
            buffer: vec![0u8; buffer_size],
        }
    }

    fn hash(&mut self, path: &PathBuf) -> io::Result<u32> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(false)
            .create(false)
            .open(path)?;

        self.digest.reset();
        loop {
            let read = file.read(&mut self.buffer)?;
            if read == 0 {
                break;
            }
            self.digest.write(&self.buffer[..read]);
        }
        Ok(self.digest.sum32())
    }
}

thread_local!(static THREAD_LOCAL_FAST_HASHER: RefCell<FastHasher> =
    RefCell::new(FastHasher::new(4 * 1024)));

pub fn fast_hash(path: &PathBuf) -> io::Result<u32> {
    THREAD_LOCAL_FAST_HASHER.with(|hasher| hasher.borrow_mut().hash(&path))
}
