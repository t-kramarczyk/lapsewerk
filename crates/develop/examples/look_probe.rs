//! Which frames of a sequence give a camera look fitted to their own JPEG, and its tone.
//!
//! `cargo run --release -p lapsewerk-develop --example look_probe -- DIR [FIRST LAST]`

#![allow(clippy::unwrap_used, clippy::expect_used)]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().expect("folder");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("arw")))
        .collect();
    paths.sort();
    let first: usize = args.get(1).map_or(0, |s| s.parse().unwrap());
    let last: usize = args.get(2).map_or(paths.len() - 1, |s| s.parse().unwrap());
    for (i, p) in paths.iter().enumerate().take(last + 1).skip(first) {
        let bytes = std::fs::read(p).unwrap();
        let look = lightcraft_engine::files::camera_look_of(&bytes).unwrap();
        let name = p.file_name().unwrap().to_string_lossy();
        match look {
            Some(l) => println!("{i} {name} look tone={:?}", l.tone),
            None => println!("{i} {name} none"),
        }
    }
}
