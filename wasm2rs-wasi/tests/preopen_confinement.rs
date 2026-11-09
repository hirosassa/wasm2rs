//! A preopen confines the guest even when the directory tree changes under the host: a
//! descriptor stays on the directory it opened, and a path is resolved and used in one step.
//! Driven through the `Host` trait directly.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use wasm2rs_wasi::wasi_filesystem_types::{
    Descriptor, DescriptorFlags, ErrorCode as Fs, OpenFlags, PathFlags,
};
use wasm2rs_wasi::{Access, Host, WasiHost};

struct Tree {
    root: PathBuf,
    outside: PathBuf,
}

/// `<scratch>/root` (the preopen, writable) and `<scratch>/outside`, which holds `secret`.
fn tree(name: &str) -> Tree {
    let base = std::env::temp_dir().join(format!(
        "wasm2rs-wasi-confinement-{}-{name}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&base).ok();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"outside").unwrap();
    Tree { root, outside }
}

fn host(tree: &Tree) -> WasiHost {
    let builder = WasiHost::builder().preopen("/", &tree.root, Access::ReadWrite);
    #[cfg(not(feature = "os-random"))]
    let builder = builder.random(NoRandom);
    builder.build().unwrap()
}

#[cfg(not(feature = "os-random"))]
struct NoRandom;
#[cfg(not(feature = "os-random"))]
impl wasm2rs_wasi::RandomSource for NoRandom {
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), wasm2rs_wasi::RandomError> {
        buf.fill(0);
        Ok(())
    }
    fn insecure_u64(&mut self) -> u64 {
        0
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (0, 0)
    }
}

const ROOT: Descriptor = Descriptor(0);

fn open(host: &mut WasiHost, dir: Descriptor, path: &str) -> Result<Descriptor, Fs> {
    host.wasi_filesystem_types_method_descriptor_open_at(
        dir,
        PathFlags::SYMLINK_FOLLOW,
        path.into(),
        OpenFlags(0),
        DescriptorFlags::READ,
    )
}

fn open_dir(host: &mut WasiHost, dir: Descriptor, path: &str) -> Result<Descriptor, Fs> {
    host.wasi_filesystem_types_method_descriptor_open_at(
        dir,
        PathFlags::SYMLINK_FOLLOW,
        path.into(),
        OpenFlags::DIRECTORY,
        DescriptorFlags::READ,
    )
}

fn read_all(host: &mut WasiHost, file: Descriptor) -> Result<String, Fs> {
    let (data, _) = host.wasi_filesystem_types_method_descriptor_read(file, 1024, 0)?;
    Ok(String::from_utf8(data).unwrap())
}

fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn open_through_a_symlink_to_outside_is_refused() {
    let t = tree("open");
    symlink(&t.outside, &t.root.join("dir-link"));
    symlink(&t.outside.join("secret"), &t.root.join("file-link"));
    let mut host = host(&t);
    assert_eq!(
        open(&mut host, ROOT, "dir-link/secret"),
        Err(Fs::NotPermitted)
    );
    assert_eq!(open(&mut host, ROOT, "file-link"), Err(Fs::NotPermitted));
    assert_eq!(open_dir(&mut host, ROOT, "dir-link"), Err(Fs::NotPermitted));
}

#[test]
fn rename_and_link_through_a_symlink_to_outside_are_refused() {
    let t = tree("rename-link");
    symlink(&t.outside, &t.root.join("out"));
    std::fs::write(t.root.join("f"), b"inside").unwrap();
    let mut host = host(&t);
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_rename_at(
            ROOT,
            "f".into(),
            ROOT,
            "out/f".into()
        ),
        Err(Fs::NotPermitted)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_rename_at(
            ROOT,
            "out/secret".into(),
            ROOT,
            "stolen".into()
        ),
        Err(Fs::NotPermitted)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_link_at(
            ROOT,
            PathFlags::SYMLINK_FOLLOW,
            "f".into(),
            ROOT,
            "out/f".into()
        ),
        Err(Fs::NotPermitted)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_link_at(
            ROOT,
            PathFlags::SYMLINK_FOLLOW,
            "out/secret".into(),
            ROOT,
            "stolen".into()
        ),
        Err(Fs::NotPermitted)
    );
    assert!(t.root.join("f").exists());
    assert!(!t.outside.join("f").exists());
    assert!(!t.root.join("stolen").exists());
    assert_eq!(std::fs::read(t.outside.join("secret")).unwrap(), b"outside");
}

#[test]
fn creating_through_a_symlink_to_outside_writes_nothing_there() {
    let t = tree("create");
    symlink(&t.outside, &t.root.join("out"));
    let mut host = host(&t);
    let created = host.wasi_filesystem_types_method_descriptor_open_at(
        ROOT,
        PathFlags::SYMLINK_FOLLOW,
        "out/new".into(),
        OpenFlags::CREATE,
        DescriptorFlags::WRITE,
    );
    assert_eq!(created, Err(Fs::NotPermitted));
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(ROOT, "out/d".into()),
        Err(Fs::NotPermitted)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_symlink_at(ROOT, "x".into(), "out/l".into()),
        Err(Fs::NotPermitted)
    );
    assert!(!t.outside.join("new").exists());
    assert!(!t.outside.join("d").exists());
    assert!(!t.outside.join("l").exists());
}

#[test]
fn a_directory_descriptor_stays_on_its_directory_when_its_path_becomes_a_symlink_to_outside() {
    let t = tree("moved-dir");
    std::fs::create_dir(t.root.join("sub")).unwrap();
    std::fs::write(t.root.join("sub").join("f"), b"inner").unwrap();
    std::fs::write(t.root.join("sub").join("secret"), b"inner secret").unwrap();
    let mut host = host(&t);
    let sub = open_dir(&mut host, ROOT, "sub").unwrap();

    std::fs::rename(t.root.join("sub"), t.root.join("sub-moved")).unwrap();
    symlink(&t.outside, &t.root.join("sub"));

    let file = open(&mut host, sub, "secret").unwrap();
    assert_eq!(read_all(&mut host, file).unwrap(), "inner secret");
    let file = open(&mut host, sub, "f").unwrap();
    assert_eq!(read_all(&mut host, file).unwrap(), "inner");
    // writing lands in the directory that was opened, not in what its old path leads to now
    let made = host.wasi_filesystem_types_method_descriptor_open_at(
        sub,
        PathFlags::SYMLINK_FOLLOW,
        "made".into(),
        OpenFlags::CREATE,
        DescriptorFlags::WRITE,
    );
    assert!(made.is_ok());
    assert!(t.root.join("sub-moved").join("made").exists());
    assert!(!t.outside.join("made").exists());
}

#[test]
fn an_intermediate_directory_swapped_for_a_symlink_does_not_redirect_a_descriptor_below_it() {
    let t = tree("moved-ancestor");
    std::fs::create_dir_all(t.root.join("a").join("b")).unwrap();
    std::fs::write(t.root.join("a").join("b").join("f"), b"inner").unwrap();
    std::fs::create_dir(t.outside.join("b")).unwrap();
    std::fs::write(t.outside.join("b").join("f"), b"outside").unwrap();
    let mut host = host(&t);
    let b = open_dir(&mut host, ROOT, "a/b").unwrap();

    std::fs::rename(t.root.join("a"), t.root.join("a-moved")).unwrap();
    symlink(&t.outside, &t.root.join("a"));

    let file = open(&mut host, b, "f").unwrap();
    assert_eq!(read_all(&mut host, file).unwrap(), "inner");
}

#[test]
fn a_symlink_flipped_between_inside_and_outside_never_opens_outside() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let t = tree("race");
    std::fs::create_dir(t.root.join("inner")).unwrap();
    std::fs::write(t.root.join("inner").join("secret"), b"inner").unwrap();
    let mut host = host(&t);
    let stop = AtomicBool::new(false);
    let swap = t.root.join("swap");
    let tmp = t.root.join("swap.tmp");
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut to_outside = true;
            while !stop.load(Ordering::Relaxed) {
                let target = if to_outside {
                    &t.outside
                } else {
                    &t.root.join("inner")
                };
                std::fs::remove_file(&tmp).ok();
                symlink(target, &tmp);
                // an atomic replace: the link always exists
                std::fs::rename(&tmp, &swap).unwrap();
                to_outside = !to_outside;
            }
        });
        // the link must exist before the first attempt
        while !swap.exists() && !swap.is_symlink() {
            std::thread::yield_now();
        }
        for _ in 0..20_000 {
            if let Ok(file) = open(&mut host, ROOT, "swap/secret") {
                let read = read_all(&mut host, file).unwrap();
                assert_ne!(read, "outside", "a path led out of the preopen");
                host.wasi_filesystem_types_resource_drop_descriptor(file);
            }
        }
        stop.store(true, Ordering::Relaxed);
    });
}
