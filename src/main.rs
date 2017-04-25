#![cfg_attr(feature="clippy", feature(plugin))]
#![cfg_attr(feature="clippy", plugin(clippy))]

#[macro_use]
extern crate log;
#[macro_use]
extern crate clap;
extern crate jobsteal;
extern crate crc;
extern crate chashmap;
extern crate crossbeam;
extern crate zip;

use std::fs::{self, ReadDir, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::fmt::Write;
use std::ffi::OsStr;

use clap::{Arg, App};

use jobsteal::{make_pool, Spawner, Pool};

use chashmap::CHashMap;

use crossbeam::sync::MsQueue;

mod fasthasher;
use fasthasher::fast_hash;

#[derive(Debug, Clone)]
enum FilePaths {
    Default(PathBuf),
    Zip(PathBuf, String),
}

#[derive(Debug)]
struct MsQueueWithPeak<T> {
    queue: MsQueue<T>,
    buffer: Option<T>,
}

impl<T> MsQueueWithPeak<T> {
    fn from(queue: MsQueue<T>) -> MsQueueWithPeak<T> {
        MsQueueWithPeak {
            queue: queue,
            buffer: Option::None,
        }
    }

    fn has_more_than_one_element(&mut self) -> bool {
        match self.buffer {
            Option::None => {
                if self.queue.is_empty() {
                    return false;
                }
                self.buffer = self.queue.try_pop();
                self.has_more_than_one_element()
            }
            Option::Some(_) => !self.queue.is_empty(),
        }
    }
}

impl<T: std::clone::Clone> std::iter::Iterator for MsQueueWithPeak<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        match self.buffer {
            Option::None => self.queue.try_pop(),
            Option::Some(_) => self.buffer.take(),
        }
    }
}

fn walk_path(scope: &Spawner, path: &PathBuf, map: Arc<CHashMap<u64, MsQueue<FilePaths>>>) {
    if path.is_dir() {
        let read_dir = fs::read_dir(path).unwrap();
        scope.recurse(|subscope| walk_dir(subscope, read_dir, map));
    } else {
        let metadata = fs::metadata(path).unwrap();
        let file_size = metadata.len();
        trace!("Walked to file: {:?} ({:010}bytes)", path, file_size);
        insert_into(&map, file_size, &FilePaths::Default(path.clone()));

        if path.extension().unwrap_or(OsStr::new("")) == OsStr::new("zip") {
            let file = OpenOptions::new()
                .read(true)
                .write(false)
                .create(false)
                .open(path)
                .unwrap();
            let mut zip = zip::ZipArchive::new(file).unwrap();
            for index in 0..zip.len() {
                let entry = zip.by_index(index).unwrap();
                insert_into(&map,
                            entry.size(),
                            &FilePaths::Zip(path.clone(), entry.name().to_string()));
            }
        }
    }
}

fn insert_into(map: &Arc<CHashMap<u64, MsQueue<FilePaths>>>, file_size: u64, path: &FilePaths) {
    map.upsert(file_size,
               || {
                   let q = MsQueue::new();
                   q.push(path.clone());
                   q
               },
               |q| q.push(path.clone()));
}

fn walk_dir(scope: &Spawner, root_dir: ReadDir, map: Arc<CHashMap<u64, MsQueue<FilePaths>>>) {
    for entry in root_dir {
        let dir = entry.unwrap();
        let path_buf = dir.path();
        let submap = map.clone();
        scope.recurse(move |subscope| walk_path(subscope, &path_buf, submap));
    }
}

fn build_size_map(pool: &mut Pool, root_paths: Vec<PathBuf>) -> CHashMap<u64, MsQueue<FilePaths>> {
    let size_map: Arc<CHashMap<u64, MsQueue<FilePaths>>> = Arc::new(CHashMap::new());
    pool.scope(|scope| for path in root_paths {
                   let sub_size_map = size_map.clone();
                   scope.recurse(move |subscope| walk_path(subscope, &path, sub_size_map));
               });

    Arc::try_unwrap(size_map).unwrap()
}

fn build_size_by_hash_map(pool: &mut Pool,
                          size_map: CHashMap<u64, MsQueue<FilePaths>>)
                          -> CHashMap<u64, CHashMap<u32, MsQueue<FilePaths>>> {
    let size_by_hash_map: Arc<CHashMap<u64, CHashMap<u32, MsQueue<FilePaths>>>> =
        Arc::new(CHashMap::new());

    pool.scope(|scope| for (size, files) in size_map {
                   let mut files = MsQueueWithPeak::from(files);
                   if !files.has_more_than_one_element() {
                       continue;
                   }
                   size_by_hash_map.insert_new(size, CHashMap::new());
                   for file in files {
                       match file {
                           FilePaths::Zip(filepath, zip_filename) => {
                let zip_file = OpenOptions::new()
                    .read(true)
                    .write(false)
                    .create(false)
                    .open(&filepath)
                    .unwrap();
                let mut zip = zip::ZipArchive::new(zip_file).unwrap();
                let hash = zip.by_name(zip_filename.as_str()).unwrap().crc32();
                let scoped_map = size_by_hash_map.clone();
                insert_into2(&scoped_map,
                             size,
                             hash,
                             FilePaths::Zip(filepath.clone(), zip_filename.clone()));
            }
                           FilePaths::Default(filepath) => {
                let scoped_map = size_by_hash_map.clone();
                scope.submit(move || {
                                 let hash = fast_hash(&filepath).unwrap();
                                 insert_into2(&scoped_map,
                                              size,
                                              hash,
                                              FilePaths::Default(filepath.clone()));
                             });
            }
                       }
                   }
               });

    Arc::try_unwrap(size_by_hash_map).unwrap()
}

fn insert_into2(map: &Arc<CHashMap<u64, CHashMap<u32, MsQueue<FilePaths>>>>,
                file_size: u64,
                hash: u32,
                path: FilePaths) {
    map.get(&file_size)
        .unwrap()
        .upsert(hash,
                || {
                    let q = MsQueue::new();
                    q.push(path.clone());
                    q
                },
                |q| q.push(path.clone()));
}

fn fmt_queue<T: std::fmt::Debug + std::clone::Clone>(queue: &mut MsQueueWithPeak<T>) -> String {
    let mut s = String::new();
    write!(s, "[").unwrap();
    let mut is_first = true;
    for element in queue {
        if is_first {
            write!(&mut s, "{:?}", element).unwrap();
            is_first = false;
        } else {
            write!(&mut s, ", {:?}", element).unwrap();
        }
    }
    write!(&mut s, "]").unwrap();
    s
}

fn main() {
    let cmd_line_args = App::new("myapp")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Look for duplicate files")
        .arg(Arg::with_name("ROOT_PATH")
                 .takes_value(true)
                 .multiple(true)
                 .empty_values(false)
                 .required(true))
        .get_matches_safe()
        .unwrap_or_else(|e| e.exit());

    let root_paths = cmd_line_args
        .values_of("ROOT_PATH")
        .unwrap()
        .map(|input| Path::new(input).to_path_buf())
        .collect::<Vec<PathBuf>>();

    let mut pool = make_pool(4).unwrap();
    let size_map = build_size_map(&mut pool, root_paths);
    let build_size_by_hash_map = build_size_by_hash_map(&mut pool, size_map);
    for (size, hash_map) in build_size_by_hash_map.into_iter() {
        for (hash, files) in hash_map.into_iter() {
            let mut files = MsQueueWithPeak::from(files);
            if !files.has_more_than_one_element() {
                continue;
            }
            println!("{:010X} - {:08X} - {:}", size, hash, fmt_queue(&mut files));
        }
    }
}


#[cfg(test)]
mod tests {
    extern crate env_logger;

    use super::*;
    use super::fmt_queue;

    #[test]
    fn has_more_than_one_element_empty() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(p.has_more_than_one_element(), false);
        assert_eq!(p.has_more_than_one_element(), false);
    }

    #[test]
    fn has_more_than_one_element_single() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        q.push(0);
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(p.has_more_than_one_element(), false);
        assert_eq!(p.has_more_than_one_element(), false);
    }

    #[test]
    fn has_more_than_one_element_multiple() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        q.push(0);
        q.push(1);
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(p.has_more_than_one_element(), true);
        assert_eq!(p.has_more_than_one_element(), true);
    }

    #[test]
    fn has_more_than_one_element_multiple_and_itered() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        q.push(0);
        q.push(1);
        q.push(2);
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(p.has_more_than_one_element(), true);
        p.next().unwrap();
        assert_eq!(p.has_more_than_one_element(), true);
        p.next().unwrap();
        assert_eq!(p.has_more_than_one_element(), false);
    }

    #[test]
    fn fmt_queue_empty() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(fmt_queue(&mut p), "[]");
    }

    #[test]
    fn fmt_queue_empty_single() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        q.push(0);
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(fmt_queue(&mut p), "[0]");
    }

    #[test]
    fn fmt_queue_empty_multiple() {
        let _ = env_logger::init();

        let q: MsQueue<u8> = MsQueue::new();
        q.push(0);
        q.push(1);
        let mut p = MsQueueWithPeak::from(q);
        assert_eq!(fmt_queue(&mut p), "[0, 1]");
    }
}
