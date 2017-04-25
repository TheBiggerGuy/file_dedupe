use std::result::Result::Ok;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::fs::{self, ReadDir, OpenOptions};
use std::cell::RefCell;

use crc::crc64::{self, Hasher64};

struct FastHasher {
    digest: crc64::Digest,
    buffer: Vec<u8>,
}

impl FastHasher {
    fn new(buffer_size: usize) -> FastHasher {
        FastHasher {
            digest: crc64::Digest::new(crc64::ISO),
            buffer: vec![0u8; buffer_size],
        }
    }

    fn hash(&mut self, path: &PathBuf) -> io::Result<u64> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(false)
            .create(false)
            .open(path)?;

        self.digest.reset();
        self.buffer.clear();
        loop {
            let read = file.read(&mut self.buffer)?;
            if read == 0 {
                break;
            }
            println!("{:?}", &self.buffer[..read]);
            self.digest.write(&self.buffer[..read]);
        }
        Ok(self.digest.sum64())
    }
}

thread_local!(static THREAD_LOCAL_FAST_HASHER: RefCell<FastHasher> =
    RefCell::new(FastHasher::new(4 * 1024)));

pub fn fast_hash(path: &PathBuf) -> io::Result<u64> {
    THREAD_LOCAL_FAST_HASHER.with(|hasher| hasher.borrow_mut().hash(&path))
}
