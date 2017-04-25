#![cfg_attr(feature="clippy", feature(plugin))]
#![cfg_attr(feature="clippy", plugin(clippy))]

#[macro_use]
extern crate clap;
extern crate jobsteal;
extern crate crc;

use std::fs::{self, ReadDir, OpenOptions};
use std::path::{Path, PathBuf};
use std::process;
use std::io::{self, Read};
use std::result::Result::Ok;
use std::collections::HashMap;
use std::ops::FnOnce;

use clap::{Arg, App};

use jobsteal::{make_pool, Spawner, Pool};

mod fasthasher;
use fasthasher::fast_hash;

struct PathsWalker<'pool> {
    root_paths: Vec<PathBuf>,
    pool: &'pool Pool,
}

impl<'pool> PathsWalker<'pool> {
    fn new(root_paths: Vec<PathBuf>, workers: usize) -> PathsWalker<'pool> {
        let pool = make_pool(workers).unwrap();
        PathsWalker {
            root_paths: root_paths,
            pool: &pool,
        }
    }

    fn walk_with<'scope, F>(&self, visitor: &'scope F)
        where F: PathsWalkerVisitor
    {
        self.pool
            .scope(move |spawner| for path in self.root_paths {
                       spawner.recurse(|subscope| PathsWalker::walk_path(subscope, &path, visitor));
                   });
    }

    fn walk_path<'scope, 'subscope, F>(scope: &'scope Spawner,
                                       path: &'subscope PathBuf,
                                       visitor: &'scope F)
        where F: PathsWalkerVisitor
    {
        if path.is_dir() {
            let read_dir = fs::read_dir(path).unwrap();
            scope.recurse(|subscope| PathsWalker::walk_dir(subscope, &read_dir, visitor));
        } else {
            scope.submit(move || visitor.visit(&path));
        }
    }

    fn walk_dir<'scope, 'subscope, F>(scope: &'scope Spawner,
                                      root_dir: &'scope ReadDir,
                                      visitor: &'scope F)
        where F: PathsWalkerVisitor
    {
        for entry in *root_dir {
            let dir = entry.unwrap();
            let path_buf: &'subscope PathBuf = &dir.path();
            scope.recurse(move |subscope| PathsWalker::walk_path(subscope, path_buf, visitor));
        }
    }
}

trait PathsWalkerVisitor: std::marker::Send + std::marker::Sync {
    fn visit(&mut self, file: &PathBuf);
}

struct PrintToConsoleVisitor {}

impl PathsWalkerVisitor for PrintToConsoleVisitor {
    fn visit(&mut self, file: &PathBuf) {
        println!("{:?}", file);
    }
}

struct MapByFastHashVisitor {
    map: HashMap<u64, Vec<PathBuf>>,
}

impl PathsWalkerVisitor for MapByFastHashVisitor {
    fn visit(&mut self, file: &PathBuf) {
        let hash = fast_hash(&file).unwrap();
        self.map
            .entry(hash)
            .or_insert(vec![])
            .push(file.clone());
    }
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

    let walker = PathsWalker::new(root_paths, 4);
    walker.walk_with(&PrintToConsoleVisitor {});

    let map_by_size = MapByFastHashVisitor { map: HashMap::new() };
    walker.walk_with(&map_by_size);
    println!("Map of size: {:}", map_by_size.map.len());
}


#[cfg(test)]
mod tests {
    extern crate env_logger;

    use super::*;

    #[test]
    fn from_range_simple_netmask() {
        let _ = env_logger::init();

        let visitor = MapByFastHashVisitor { map: HashMap::new() };

        visitor.visit(&Path::new("file1.txt").to_path_buf());

        assert_eq!(visitor.map.len(), 1);
    }
}
