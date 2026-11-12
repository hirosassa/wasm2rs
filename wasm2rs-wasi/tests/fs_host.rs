//! The filesystem half of `WasiHost`, driven through the `Host` trait: the error code of each
//! refusal, offsets and sizes at their limits, the read-only contract of a preopen, and what
//! streams over a descriptor see. Real files in a scratch directory; nothing is mocked.

mod common;

use common::{builder, scratch, traps};
use std::path::Path;
#[cfg(unix)]
use std::time::UNIX_EPOCH;
use wasm2rs_wasi::wasi_clocks_wall_clock::Datetime;
use wasm2rs_wasi::wasi_filesystem_types::{
    Advice, Descriptor, DescriptorFlags, DescriptorType, ErrorCode as Fs, NewTimestamp, OpenFlags,
    PathFlags,
};
use wasm2rs_wasi::wasi_io_streams::StreamError;
use wasm2rs_wasi::{Access, Host, WasiHost};

const ROOT: Descriptor = Descriptor(0);
const FOLLOW: PathFlags = PathFlags::SYMLINK_FOLLOW;
const RW: DescriptorFlags = DescriptorFlags(DescriptorFlags::READ.0 | DescriptorFlags::WRITE.0);

fn host_over(root: &Path, access: Access) -> WasiHost {
    builder().preopen("/", root, access).build().unwrap()
}

fn open(
    host: &mut WasiHost,
    path: &str,
    open_flags: OpenFlags,
    flags: DescriptorFlags,
) -> Result<Descriptor, Fs> {
    host.wasi_filesystem_types_method_descriptor_open_at(
        ROOT,
        FOLLOW,
        path.into(),
        open_flags,
        flags,
    )
}

fn create(host: &mut WasiHost, path: &str) -> Descriptor {
    open(host, path, OpenFlags::CREATE, RW).unwrap()
}

fn read_at(host: &mut WasiHost, d: Descriptor, length: u64, offset: u64) -> (Vec<u8>, bool) {
    host.wasi_filesystem_types_method_descriptor_read(d, length, offset)
        .unwrap()
}

fn timestamp(seconds: u64, nanoseconds: u32) -> NewTimestamp {
    NewTimestamp::Timestamp(Datetime {
        seconds,
        nanoseconds,
    })
}

// ---- positional read / write ------------------------------------------------------------

#[test]
fn a_write_past_the_end_zero_fills_the_gap_and_a_read_reports_the_end() {
    let root = scratch("gap");
    let mut host = host_over(&root, Access::ReadWrite);
    let f = create(&mut host, "f");
    let written = host
        .wasi_filesystem_types_method_descriptor_write(f, b"xy".to_vec(), 3)
        .unwrap();
    assert_eq!(written, 2);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"\0\0\0xy");
    // exactly the file's length: all of it, and not yet "the end" (no read hit EOF)
    assert_eq!(read_at(&mut host, f, 5, 0), (b"\0\0\0xy".to_vec(), false));
    // asking for more reaches the end
    assert_eq!(read_at(&mut host, f, 6, 0), (b"\0\0\0xy".to_vec(), true));
    // at and past the end: empty, and the end
    assert_eq!(read_at(&mut host, f, 4, 5), (vec![], true));
    assert_eq!(read_at(&mut host, f, 4, 1 << 40), (vec![], true));
    // a zero-length read is not the end of anything
    assert_eq!(read_at(&mut host, f, 0, 0), (vec![], false));
}

#[test]
fn a_read_returns_at_most_one_mebibyte_per_call() {
    let root = scratch("limit");
    std::fs::write(root.join("big"), vec![7u8; (1 << 20) + 10]).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let f = open(&mut host, "big", OpenFlags(0), DescriptorFlags::READ).unwrap();
    let (first, eof) = read_at(&mut host, f, u64::MAX, 0);
    assert_eq!((first.len(), eof), (1 << 20, false));
    let (rest, eof) = read_at(&mut host, f, u64::MAX, 1 << 20);
    assert_eq!((rest.len(), eof), (10, true));
}

#[test]
fn an_offset_beyond_what_a_file_can_address_is_an_error_not_a_panic() {
    let root = scratch("offset-max");
    let mut host = host_over(&root, Access::ReadWrite);
    let f = create(&mut host, "f");
    std::fs::write(root.join("f"), b"abc").unwrap();
    // `u64::MAX` does not fit a file offset: the OS refuses the seek (reported as the
    // catch-all `io`, which this pins; the contract is only "an error, not a panic")
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read(f, 1, u64::MAX),
        Err(Fs::Io)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write(f, b"z".to_vec(), u64::MAX),
        Err(Fs::Io)
    );
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"abc");
}

#[test]
fn descriptor_flags_decide_reading_and_writing() {
    let root = scratch("flags");
    std::fs::write(root.join("f"), b"data").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);

    let ro = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    let wo = open(&mut host, "f", OpenFlags(0), DescriptorFlags::WRITE).unwrap();
    let get = |host: &mut WasiHost, d| host.wasi_filesystem_types_method_descriptor_get_flags(d);
    assert_eq!(get(&mut host, ro), Ok(DescriptorFlags::READ));
    assert_eq!(get(&mut host, wo), Ok(DescriptorFlags::WRITE));
    assert_eq!(get(&mut host, ROOT), Ok(DescriptorFlags::READ));

    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write(ro, b"x".to_vec(), 0),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_size(ro, 0),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read(wo, 1, 0),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read_via_stream(wo, 0),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write_via_stream(ro, 0),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_append_via_stream(ro),
        Err(Fs::BadDescriptor)
    );
    // a directory is no file
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read(ROOT, 1, 0),
        Err(Fs::IsDirectory)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write(ROOT, b"x".to_vec(), 0),
        Err(Fs::IsDirectory)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_advise(ROOT, 0, 0, Advice::Normal),
        Err(Fs::IsDirectory)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_advise(ro, 0, 0, Advice::Normal),
        Ok(())
    );
}

#[test]
fn set_size_truncates_and_zero_extends() {
    let root = scratch("set-size");
    std::fs::write(root.join("f"), b"abcdef").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();
    host.wasi_filesystem_types_method_descriptor_set_size(f, 2)
        .unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"ab");
    host.wasi_filesystem_types_method_descriptor_set_size(f, 4)
        .unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"ab\0\0");
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_stat(f)
            .unwrap()
            .size,
        4
    );
}

// ---- open-at ----------------------------------------------------------------------------

#[test]
fn open_at_answers_each_conflict_with_its_own_code() {
    let root = scratch("open-codes");
    std::fs::write(root.join("file"), b"x").unwrap();
    std::fs::create_dir(root.join("dir")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let none = OpenFlags(0);
    let r = DescriptorFlags::READ;

    assert_eq!(open(&mut host, "missing", none, r), Err(Fs::NoEntry));
    assert_eq!(
        open(
            &mut host,
            "file",
            OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0),
            RW
        ),
        Err(Fs::Exist)
    );
    assert_eq!(
        open(&mut host, "file", OpenFlags::DIRECTORY, r),
        Err(Fs::NotDirectory)
    );
    assert_eq!(
        open(&mut host, "dir", none, DescriptorFlags::WRITE),
        Err(Fs::IsDirectory)
    );
    assert_eq!(
        open(&mut host, "dir", OpenFlags::TRUNCATE, r),
        Err(Fs::IsDirectory)
    );
    // a directory cannot be asked for together with a change
    assert_eq!(
        open(
            &mut host,
            "dir",
            OpenFlags::DIRECTORY,
            DescriptorFlags::WRITE
        ),
        Err(Fs::Invalid)
    );
    assert_eq!(
        open(
            &mut host,
            "new",
            OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0),
            RW
        ),
        Err(Fs::Invalid)
    );
    // a path that could leave the descriptor is refused before anything is looked up
    assert_eq!(open(&mut host, "../x", none, r), Err(Fs::NotPermitted));
    assert_eq!(
        open(&mut host, "/etc/passwd", none, r),
        Err(Fs::NotPermitted)
    );
    assert_eq!(
        open(&mut host, "dir/../file", none, r),
        Err(Fs::NotPermitted)
    );
    // a file used as a directory in the middle of a path
    assert_eq!(open(&mut host, "file/x", none, r), Err(Fs::NotDirectory));
    // a new name ending in a slash is not a file name
    assert_eq!(
        open(&mut host, "new/", OpenFlags::CREATE, RW),
        Err(Fs::Invalid)
    );
}

#[test]
fn truncate_empties_and_create_makes_an_empty_file() {
    let root = scratch("truncate");
    std::fs::write(root.join("f"), b"old contents").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags::TRUNCATE, RW).unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"");
    assert_eq!(read_at(&mut host, f, 8, 0), (vec![], true));
    create(&mut host, "fresh");
    assert_eq!(std::fs::read(root.join("fresh")).unwrap(), b"");
    // `create` without `exclusive` opens what is there and leaves it alone
    std::fs::write(root.join("kept"), b"keep").unwrap();
    open(&mut host, "kept", OpenFlags::CREATE, RW).unwrap();
    assert_eq!(std::fs::read(root.join("kept")).unwrap(), b"keep");
}

#[test]
fn an_empty_path_and_dot_open_the_directory_itself() {
    let root = scratch("dot");
    std::fs::write(root.join("f"), b"x").unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let same = open(&mut host, "", OpenFlags::DIRECTORY, DescriptorFlags::READ).unwrap();
    let dot = open(&mut host, ".", OpenFlags::DIRECTORY, DescriptorFlags::READ).unwrap();
    assert!(host.wasi_filesystem_types_method_descriptor_is_same_object(ROOT, same));
    assert!(host.wasi_filesystem_types_method_descriptor_is_same_object(ROOT, dot));
    let file = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    assert!(!host.wasi_filesystem_types_method_descriptor_is_same_object(ROOT, file));
}

// ---- read-only preopens -----------------------------------------------------------------

#[test]
fn a_read_only_preopen_refuses_every_change_but_allows_reading_and_sync() {
    let root = scratch("ro");
    std::fs::write(root.join("f"), b"data").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let f = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();

    assert_eq!(
        open(&mut host, "n", OpenFlags::CREATE, DescriptorFlags::READ),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        open(&mut host, "f", OpenFlags::TRUNCATE, DescriptorFlags::READ),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        open(&mut host, "f", OpenFlags(0), DescriptorFlags::WRITE),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        open(
            &mut host,
            "f",
            OpenFlags(0),
            DescriptorFlags::MUTATE_DIRECTORY
        ),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write(f, b"x".to_vec(), 0),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_size(f, 0),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_write_via_stream(f, 0)
            .map(|_| ()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_append_via_stream(f)
            .map(|_| ()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(ROOT, "n".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_unlink_file_at(ROOT, "f".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_remove_directory_at(ROOT, "d".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_rename_at(ROOT, "f".into(), ROOT, "g".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_symlink_at(ROOT, "f".into(), "l".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_link_at(
            ROOT,
            FOLLOW,
            "f".into(),
            ROOT,
            "h".into()
        ),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times(
            f,
            NewTimestamp::Now,
            NewTimestamp::Now
        ),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times_at(
            ROOT,
            FOLLOW,
            "f".into(),
            NewTimestamp::Now,
            NewTimestamp::Now
        ),
        Err(Fs::ReadOnly)
    );
    // nothing changed
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["d", "f"]);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"data");

    // reading and flushing change nothing, so they are allowed
    assert_eq!(read_at(&mut host, f, 8, 0), (b"data".to_vec(), true));
    // (Windows cannot flush a handle that was opened read-only, nor a directory)
    #[cfg(unix)]
    {
        assert_eq!(host.wasi_filesystem_types_method_descriptor_sync(f), Ok(()));
        assert_eq!(
            host.wasi_filesystem_types_method_descriptor_sync_data(ROOT),
            Ok(())
        );
    }
}

// ---- directories ------------------------------------------------------------------------

#[test]
fn read_directory_lists_each_entry_once_with_its_type() {
    let root = scratch("list");
    std::fs::write(root.join("a"), b"1").unwrap();
    std::fs::create_dir(root.join("b")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a", root.join("c")).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let stream = host
        .wasi_filesystem_types_method_descriptor_read_directory(ROOT)
        .unwrap();
    let mut entries = Vec::new();
    while let Some(entry) = host
        .wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(stream)
        .unwrap()
    {
        entries.push((entry.name, entry.type_));
    }
    entries.sort_by(|x, y| x.0.cmp(&y.0));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut expected = vec![
        ("a".to_string(), DescriptorType::RegularFile),
        ("b".to_string(), DescriptorType::Directory),
    ];
    #[cfg(unix)]
    expected.push(("c".to_string(), DescriptorType::SymbolicLink));
    assert_eq!(entries, expected, "no `.` or `..`, each name once");
    // exhausted stays exhausted
    assert_eq!(
        host.wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(stream),
        Ok(None)
    );
    // a dropped listing is a bad descriptor
    host.wasi_filesystem_types_resource_drop_directory_entry_stream(stream);
    assert_eq!(
        host.wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(stream),
        Err(Fs::BadDescriptor)
    );
}

#[test]
fn read_directory_of_a_file_or_an_unknown_descriptor_is_refused() {
    let root = scratch("list-bad");
    std::fs::write(root.join("f"), b"1").unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let f = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read_directory(f)
            .map(|_| ()),
        Err(Fs::NotDirectory)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_read_directory(Descriptor(99))
            .map(|_| ()),
        Err(Fs::BadDescriptor)
    );
}

#[test]
fn directory_changes_report_why_they_fail() {
    let root = scratch("dir-ops");
    std::fs::create_dir(root.join("full")).unwrap();
    std::fs::write(root.join("full/x"), b"1").unwrap();
    std::fs::write(root.join("file"), b"1").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);

    let mkdir = |h: &mut WasiHost, p: &str| {
        h.wasi_filesystem_types_method_descriptor_create_directory_at(ROOT, p.into())
    };
    assert_eq!(mkdir(&mut host, "made"), Ok(()));
    assert!(root.join("made").is_dir());
    assert_eq!(mkdir(&mut host, "made"), Err(Fs::Exist));
    assert_eq!(mkdir(&mut host, "file"), Err(Fs::Exist));
    assert_eq!(mkdir(&mut host, "nope/child"), Err(Fs::NoEntry));
    assert_eq!(mkdir(&mut host, "../up"), Err(Fs::NotPermitted));
    assert_eq!(mkdir(&mut host, "dir/"), Err(Fs::Invalid));

    let rmdir = |h: &mut WasiHost, p: &str| {
        h.wasi_filesystem_types_method_descriptor_remove_directory_at(ROOT, p.into())
    };
    assert_eq!(rmdir(&mut host, "full"), Err(Fs::NotEmpty));
    assert_eq!(rmdir(&mut host, "file"), Err(Fs::NotDirectory));
    assert_eq!(rmdir(&mut host, "absent"), Err(Fs::NoEntry));
    assert_eq!(rmdir(&mut host, "made"), Ok(()));
    assert!(!root.join("made").exists());

    let unlink = |h: &mut WasiHost, p: &str| {
        h.wasi_filesystem_types_method_descriptor_unlink_file_at(ROOT, p.into())
    };
    assert_eq!(unlink(&mut host, "full"), Err(Fs::IsDirectory));
    assert_eq!(unlink(&mut host, "absent"), Err(Fs::NoEntry));
    assert_eq!(unlink(&mut host, "file"), Ok(()));
    assert!(!root.join("file").exists());
}

// ---- rename / links ---------------------------------------------------------------------

#[test]
fn rename_replaces_an_existing_file_and_keeps_the_contents() {
    let root = scratch("rename");
    std::fs::write(root.join("a"), b"from a").unwrap();
    std::fs::write(root.join("b"), b"old b").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    host.wasi_filesystem_types_method_descriptor_rename_at(ROOT, "a".into(), ROOT, "b".into())
        .unwrap();
    assert!(!root.join("a").exists());
    assert_eq!(std::fs::read(root.join("b")).unwrap(), b"from a");
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_rename_at(ROOT, "a".into(), ROOT, "c".into()),
        Err(Fs::NoEntry)
    );
}

#[cfg(unix)]
#[test]
fn symlink_at_and_readlink_at_round_trip_and_refuse_unreadable_targets() {
    let root = scratch("symlink");
    std::fs::write(root.join("f"), b"x").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let link = |h: &mut WasiHost, target: &str, name: &str| {
        h.wasi_filesystem_types_method_descriptor_symlink_at(ROOT, target.into(), name.into())
    };
    assert_eq!(link(&mut host, "f", "l"), Ok(()));
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_readlink_at(ROOT, "l".into()),
        Ok("f".to_string())
    );
    assert_eq!(link(&mut host, "f", "l"), Err(Fs::Exist));
    // a target the guest could not read back, or that leads out, is refused at creation
    assert_eq!(link(&mut host, "", "e"), Err(Fs::NoEntry));
    assert_eq!(link(&mut host, "../x", "up"), Err(Fs::NotPermitted));
    assert_eq!(link(&mut host, "/etc", "abs"), Err(Fs::NotPermitted));
    assert!(!root.join("up").exists() && !root.join("abs").exists());
    // readlink of what is no link, or is absent
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_readlink_at(ROOT, "f".into()),
        Err(Fs::Invalid)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_readlink_at(ROOT, "absent".into()),
        Err(Fs::NoEntry)
    );
    // a link made by the host side that points outside is not revealed
    std::os::unix::fs::symlink("../elsewhere", root.join("out")).unwrap();
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_readlink_at(ROOT, "out".into()),
        Err(Fs::NotPermitted)
    );
}

#[cfg(unix)]
#[test]
fn stat_at_describes_a_link_itself_without_follow_and_its_target_with_it() {
    let root = scratch("stat-at");
    std::fs::write(root.join("f"), b"12345").unwrap();
    std::os::unix::fs::symlink("f", root.join("l")).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let stat = |h: &mut WasiHost, flags, p: &str| {
        h.wasi_filesystem_types_method_descriptor_stat_at(ROOT, flags, p.into())
            .unwrap()
    };
    let through = stat(&mut host, FOLLOW, "l");
    assert_eq!(
        (through.type_, through.size),
        (DescriptorType::RegularFile, 5)
    );
    let itself = stat(&mut host, PathFlags(0), "l");
    assert_eq!(itself.type_, DescriptorType::SymbolicLink);
    // the empty path and a trailing slash name the directory, never an entry
    assert_eq!(
        stat(&mut host, PathFlags(0), "").type_,
        DescriptorType::Directory
    );
    assert_eq!(
        stat(&mut host, PathFlags(0), ".").type_,
        DescriptorType::Directory
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_stat_at(ROOT, FOLLOW, "../x".into()),
        Err(Fs::NotPermitted)
    );
    // the identity of a link and its target differ without follow, agree with it
    let hash = |h: &mut WasiHost, flags, p: &str| {
        h.wasi_filesystem_types_method_descriptor_metadata_hash_at(ROOT, flags, p.into())
            .unwrap()
    };
    assert_eq!(hash(&mut host, FOLLOW, "l"), hash(&mut host, FOLLOW, "f"));
    assert_ne!(
        hash(&mut host, PathFlags(0), "l"),
        hash(&mut host, FOLLOW, "f")
    );
}

#[test]
fn metadata_hash_names_a_file_not_a_path() {
    let root = scratch("hash");
    std::fs::write(root.join("a"), b"1").unwrap();
    std::fs::write(root.join("b"), b"1").unwrap();
    #[cfg(unix)]
    std::fs::hard_link(root.join("a"), root.join("a2")).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let ro = DescriptorFlags::READ;
    let a = open(&mut host, "a", OpenFlags(0), ro).unwrap();
    let a_again = open(&mut host, "a", OpenFlags(0), ro).unwrap();
    let b = open(&mut host, "b", OpenFlags(0), ro).unwrap();
    let hash = |h: &mut WasiHost, d| {
        h.wasi_filesystem_types_method_descriptor_metadata_hash(d)
            .unwrap()
    };
    assert_eq!(hash(&mut host, a), hash(&mut host, a_again));
    // non-Unix hosts identify a file by its path, so a hard link is a different file there
    #[cfg(unix)]
    {
        let a2 = open(&mut host, "a2", OpenFlags(0), ro).unwrap();
        assert_eq!(
            hash(&mut host, a),
            hash(&mut host, a2),
            "a hard link is the same file"
        );
        assert!(host.wasi_filesystem_types_method_descriptor_is_same_object(a, a2));
    }
    assert_ne!(hash(&mut host, a), hash(&mut host, b));
    let h = hash(&mut host, a);
    assert_ne!(
        h.lower, h.upper,
        "the two halves are not one value repeated"
    );
    assert!(!host.wasi_filesystem_types_method_descriptor_is_same_object(a, b));
    // an unknown descriptor is not the same as anything, itself included
    assert!(
        !host
            .wasi_filesystem_types_method_descriptor_is_same_object(Descriptor(99), Descriptor(99))
    );
    assert!(!host.wasi_filesystem_types_method_descriptor_is_same_object(a, Descriptor(99)));
}

#[test]
fn hard_links_share_the_file_and_refuse_directories_and_taken_names() {
    let root = scratch("hard-link");
    std::fs::write(root.join("f"), b"1").unwrap();
    std::fs::write(root.join("taken"), b"2").unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let link = |h: &mut WasiHost, flags, old: &str, new: &str| {
        h.wasi_filesystem_types_method_descriptor_link_at(ROOT, flags, old.into(), ROOT, new.into())
    };
    assert_eq!(link(&mut host, FOLLOW, "f", "g"), Ok(()));
    let g = open(&mut host, "g", OpenFlags(0), DescriptorFlags::READ).unwrap();
    // non-Unix hosts do not report a link count (always 1)
    #[cfg(unix)]
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_stat(g)
            .unwrap()
            .link_count,
        2
    );
    #[cfg(not(unix))]
    let _ = g;
    assert_eq!(link(&mut host, FOLLOW, "f", "taken"), Err(Fs::Exist));
    assert_eq!(link(&mut host, FOLLOW, "d", "d2"), Err(Fs::NotPermitted));
    assert_eq!(
        link(&mut host, PathFlags(0), "d", "d2"),
        Err(Fs::NotPermitted)
    );
    assert_eq!(link(&mut host, FOLLOW, "absent", "x"), Err(Fs::NoEntry));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("f", root.join("l")).unwrap();
        // without follow a link is not linked (hosts differ on what that would mean)
        assert_eq!(
            link(&mut host, PathFlags(0), "l", "x"),
            Err(Fs::Unsupported)
        );
    }
}

// ---- times ------------------------------------------------------------------------------

#[test]
fn set_times_sets_exactly_what_was_asked_and_leaves_no_change_alone() {
    let root = scratch("times");
    std::fs::write(root.join("f"), b"1").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();
    host.wasi_filesystem_types_method_descriptor_set_times(
        f,
        timestamp(1_000, 500),
        timestamp(2_000, 700), // NTFS keeps 100ns ticks, so not a finer value
    )
    .unwrap();
    let stat = host
        .wasi_filesystem_types_method_descriptor_stat(f)
        .unwrap();
    // (the access time is not asserted: the OS may update it on its own, as macOS does)
    assert_eq!(
        stat.data_modification_timestamp,
        Some(Datetime {
            seconds: 2_000,
            nanoseconds: 700
        })
    );
    assert_eq!(stat.status_change_timestamp, None, "never reported");
    // `no-change` leaves the modification time where it was
    host.wasi_filesystem_types_method_descriptor_set_times(
        f,
        timestamp(3_000, 0),
        NewTimestamp::NoChange,
    )
    .unwrap();
    let stat = host
        .wasi_filesystem_types_method_descriptor_stat(f)
        .unwrap();
    assert_eq!(stat.data_modification_timestamp.unwrap().seconds, 2_000);
    // through a path as well
    host.wasi_filesystem_types_method_descriptor_set_times_at(
        ROOT,
        FOLLOW,
        "f".into(),
        timestamp(5, 0),
        timestamp(6, 0),
    )
    .unwrap();
    let stat = host
        .wasi_filesystem_types_method_descriptor_stat_at(ROOT, FOLLOW, "f".into())
        .unwrap();
    assert_eq!(stat.data_modification_timestamp.unwrap().seconds, 6);
}

#[test]
fn set_times_refuses_a_second_of_nanoseconds_and_an_unrepresentable_time() {
    let root = scratch("times-invalid");
    std::fs::write(root.join("f"), b"1").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();
    // the last valid nanosecond is accepted, the next one is not
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times(
            f,
            timestamp(1, 999_999_999),
            NewTimestamp::NoChange
        ),
        Ok(())
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times(
            f,
            timestamp(1, 1_000_000_000),
            NewTimestamp::NoChange
        ),
        Err(Fs::Invalid)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times(
            f,
            NewTimestamp::NoChange,
            timestamp(u64::MAX, 999_999_999)
        ),
        Err(Fs::Invalid),
        "a time past what the platform can hold is `invalid`, not a panic"
    );
    // a file not opened for write
    let ro = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times(
            ro,
            NewTimestamp::NoChange,
            NewTimestamp::NoChange
        ),
        Err(Fs::BadDescriptor)
    );
}

// ---- streams over a descriptor ----------------------------------------------------------

#[test]
fn streams_over_one_file_keep_their_own_positions() {
    let root = scratch("streams");
    std::fs::write(root.join("f"), b"0123456789").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();

    let a = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(f, 2)
        .unwrap();
    let b = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(f, 6)
        .unwrap();
    // interleaved reads: each continues from its own offset
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(a, 2),
        Ok(b"23".to_vec())
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(b, 2),
        Ok(b"67".to_vec())
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(a, 2),
        Ok(b"45".to_vec())
    );
    // skip moves the position without returning bytes
    assert_eq!(host.wasi_io_streams_method_input_stream_skip(b, 1), Ok(1));
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(b, 8),
        Ok(b"9".to_vec())
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(b, 8),
        Err(StreamError::Closed),
        "at the end of the file"
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_skip(b, 0),
        Err(StreamError::Closed),
        "even for zero bytes"
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_blocking_skip(a, 100),
        Ok(4),
        "a was at 6 of 10"
    );
    host.wasi_io_streams_resource_drop_input_stream(a);
    host.wasi_io_streams_resource_drop_input_stream(b);
}

#[test]
fn write_via_stream_writes_at_the_offset_and_append_goes_to_the_end() {
    let root = scratch("write-streams");
    std::fs::write(root.join("f"), b"abcdef").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();

    let w = host
        .wasi_filesystem_types_method_descriptor_write_via_stream(f, 2)
        .unwrap();
    assert_eq!(
        host.wasi_io_streams_method_output_stream_check_write(w),
        Ok(4096)
    );
    host.wasi_io_streams_method_output_stream_blocking_write_and_flush(w, b"XY".to_vec())
        .unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"abXYef");
    host.wasi_io_streams_method_output_stream_write(w, b"Z".to_vec())
        .unwrap();
    host.wasi_io_streams_method_output_stream_flush(w).unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"abXYZf");

    // append ignores the position and each write lands after the previous end
    let ap = host
        .wasi_filesystem_types_method_descriptor_append_via_stream(f)
        .unwrap();
    host.wasi_io_streams_method_output_stream_write(ap, b"1".to_vec())
        .unwrap();
    host.wasi_io_streams_method_output_stream_write(ap, b"2".to_vec())
        .unwrap();
    host.wasi_io_streams_method_output_stream_write_zeroes(ap, 2)
        .unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"abXYZf12\0\0");
    host.wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush(ap, 1)
        .unwrap();
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"abXYZf12\0\0\0");
    host.wasi_io_streams_resource_drop_output_stream(w);
    host.wasi_io_streams_resource_drop_output_stream(ap);
}

#[test]
fn a_write_longer_than_check_write_traps() {
    let root = scratch("write-limit");
    let mut host = host_over(&root, Access::ReadWrite);
    let f = create(&mut host, "f");
    let w = host
        .wasi_filesystem_types_method_descriptor_write_via_stream(f, 0)
        .unwrap();
    // exactly the permit is fine
    host.wasi_io_streams_method_output_stream_write(w, vec![1; 4096])
        .unwrap();
    assert!(traps(
        || host.wasi_io_streams_method_output_stream_write(w, vec![1; 4097])
    ));
    assert_eq!(std::fs::metadata(root.join("f")).unwrap().len(), 4096);
}

#[test]
fn splice_moves_at_most_4096_bytes_between_file_streams() {
    let root = scratch("splice");
    std::fs::write(root.join("src"), vec![9u8; 5000]).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let src = open(&mut host, "src", OpenFlags(0), DescriptorFlags::READ).unwrap();
    let dst = create(&mut host, "dst");
    let input = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(src, 0)
        .unwrap();
    let output = host
        .wasi_filesystem_types_method_descriptor_write_via_stream(dst, 0)
        .unwrap();
    let moved = host
        .wasi_io_streams_method_output_stream_splice(output, input, 10_000)
        .unwrap();
    assert!(moved <= 4096, "{moved}");
    let more = host
        .wasi_io_streams_method_output_stream_blocking_splice(output, input, 10_000)
        .unwrap();
    assert_eq!(moved + more, 5000);
    assert_eq!(std::fs::read(root.join("dst")).unwrap(), vec![9u8; 5000]);
    // the source is at its end now
    assert_eq!(
        host.wasi_io_streams_method_output_stream_splice(output, input, 1),
        Err(StreamError::Closed)
    );
    // a dropped output is `closed` before the source is read
    host.wasi_io_streams_resource_drop_output_stream(output);
    let again = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(src, 0)
        .unwrap();
    assert_eq!(
        host.wasi_io_streams_method_output_stream_splice(output, again, 10),
        Err(StreamError::Closed)
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(again, 3),
        Ok(vec![9, 9, 9]),
        "nothing of the source was consumed"
    );
}

// ---- descriptor bookkeeping -------------------------------------------------------------

#[test]
fn a_dropped_descriptor_is_a_bad_descriptor_and_its_slot_is_reused() {
    let root = scratch("drop-descriptor");
    std::fs::write(root.join("f"), b"1").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let f = open(&mut host, "f", OpenFlags(0), RW).unwrap();
    host.wasi_filesystem_types_resource_drop_descriptor(f);
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_get_type(f),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_stat(f)
            .map(|_| ()),
        Err(Fs::BadDescriptor)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_sync(f),
        Err(Fs::BadDescriptor)
    );
    // an unknown handle and a file used as a directory
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_open_at(
            Descriptor(500),
            FOLLOW,
            "f".into(),
            OpenFlags(0),
            DescriptorFlags::READ,
        ),
        Err(Fs::BadDescriptor)
    );
    let g = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    assert_eq!(g.0, f.0, "the freed slot is the next one used");
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_get_type(g),
        Ok(DescriptorType::RegularFile)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_get_type(ROOT),
        Ok(DescriptorType::Directory)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(g, "x".into()),
        Err(Fs::NotDirectory)
    );
}

// ---- symlinks as the last component, without `symlink-follow` ---------------------------

#[cfg(unix)]
#[test]
fn without_follow_a_symlink_name_is_loop_and_create_exclusive_never_follows() {
    let root = scratch("nofollow");
    std::fs::write(root.join("target"), b"t").unwrap();
    std::os::unix::fs::symlink("target", root.join("link")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    let open_nofollow = |h: &mut WasiHost, p: &str, flags, d| {
        h.wasi_filesystem_types_method_descriptor_open_at(ROOT, PathFlags(0), p.into(), flags, d)
    };
    let r = DescriptorFlags::READ;
    assert_eq!(
        open_nofollow(&mut host, "link", OpenFlags(0), r),
        Err(Fs::Loop)
    );
    assert_eq!(
        open_nofollow(&mut host, "dangling", OpenFlags(0), r),
        Err(Fs::Loop)
    );
    assert_eq!(
        open_nofollow(&mut host, "link", OpenFlags::CREATE, RW),
        Err(Fs::Loop)
    );
    // create + exclusive: the name exists, dangling or not, with or without follow
    let excl = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    assert_eq!(open_nofollow(&mut host, "link", excl, RW), Err(Fs::Exist));
    assert_eq!(
        open_nofollow(&mut host, "dangling", excl, RW),
        Err(Fs::Exist)
    );
    assert_eq!(open(&mut host, "dangling", excl, RW), Err(Fs::Exist));
    // with follow, creating through a dangling link is refused, not done at its target
    assert_eq!(
        open(&mut host, "dangling", OpenFlags::CREATE, RW),
        Err(Fs::NotPermitted)
    );
    assert!(!root.join("nowhere").exists());
    // a plain file without follow is opened as ever
    assert!(open_nofollow(&mut host, "target", OpenFlags(0), r).is_ok());
}

#[cfg(unix)]
#[test]
fn set_times_at_without_follow_changes_a_file_but_not_a_symlink() {
    let root = scratch("times-nofollow");
    std::fs::write(root.join("f"), b"1").unwrap();
    std::os::unix::fs::symlink("f", root.join("l")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    // a symlink's own times cannot be set through `std`
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_set_times_at(
            ROOT,
            PathFlags(0),
            "l".into(),
            timestamp(1, 0),
            timestamp(2, 0)
        ),
        Err(Fs::Unsupported)
    );
    host.wasi_filesystem_types_method_descriptor_set_times_at(
        ROOT,
        PathFlags(0),
        "f".into(),
        timestamp(11, 0),
        timestamp(12, 0),
    )
    .unwrap();
    let stat = host
        .wasi_filesystem_types_method_descriptor_stat_at(ROOT, PathFlags(0), "f".into())
        .unwrap();
    assert_eq!(stat.data_modification_timestamp.unwrap().seconds, 12);
    // through the link with follow it is the target that changes
    host.wasi_filesystem_types_method_descriptor_set_times_at(
        ROOT,
        FOLLOW,
        "l".into(),
        timestamp(21, 0),
        timestamp(22, 0),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(root.join("f"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        22
    );
}

#[test]
fn a_zero_length_file_stream_read_is_ok_before_the_end_and_closed_at_it() {
    let root = scratch("zero-read");
    std::fs::write(root.join("f"), b"ab").unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let f = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    let s = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(f, 0)
        .unwrap();
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(s, 0),
        Ok(vec![])
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_blocking_read(s, 8),
        Ok(b"ab".to_vec())
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(s, 0),
        Err(StreamError::Closed)
    );
    // a file stream's subscription is ready at once
    let p = host.wasi_io_streams_method_input_stream_subscribe(s);
    assert!(host.wasi_io_poll_method_pollable_ready(p));
    // a stream started at the end, or far past it, is closed on its first read
    let past = host
        .wasi_filesystem_types_method_descriptor_read_via_stream(f, 1 << 40)
        .unwrap();
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(past, 4),
        Err(StreamError::Closed)
    );
}

// ---- open-at: flags and error mapping ----------------------------------------------------

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_no_entry_unless_a_file_is_to_be_created_through_it() {
    let root = scratch("dangling");
    std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    assert_eq!(
        open(&mut host, "dangling", OpenFlags(0), DescriptorFlags::READ),
        Err(Fs::NoEntry)
    );
    assert_eq!(
        open(&mut host, "dangling", OpenFlags::CREATE, RW),
        Err(Fs::NotPermitted)
    );
}

#[test]
fn create_and_truncate_work_without_the_write_descriptor_flag() {
    let root = scratch("flags-without-write");
    std::fs::write(root.join("old"), b"contents").unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    open(&mut host, "new", OpenFlags::CREATE, DescriptorFlags::READ).unwrap();
    assert_eq!(std::fs::read(root.join("new")).unwrap(), b"");
    open(&mut host, "old", OpenFlags::TRUNCATE, DescriptorFlags::READ).unwrap();
    assert_eq!(std::fs::read(root.join("old")).unwrap(), b"");
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_access_whether_or_not_it_is_reached_through_a_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("unreadable");
    std::fs::write(root.join("secret"), b"x").unwrap();
    std::fs::set_permissions(root.join("secret"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::os::unix::fs::symlink("secret", root.join("link")).unwrap();
    let mut host = host_over(&root, Access::ReadWrite);
    // a process that can read it anyway (root) has nothing to refuse
    if std::fs::File::open(root.join("secret")).is_ok() {
        return;
    }
    let nofollow = |host: &mut WasiHost, path: &str| {
        host.wasi_filesystem_types_method_descriptor_open_at(
            ROOT,
            PathFlags(0),
            path.into(),
            OpenFlags(0),
            DescriptorFlags::READ,
        )
    };
    assert_eq!(nofollow(&mut host, "secret"), Err(Fs::Access));
    assert_eq!(
        open(&mut host, "secret", OpenFlags(0), DescriptorFlags::READ),
        Err(Fs::Access)
    );
    assert_eq!(
        open(&mut host, "link", OpenFlags(0), DescriptorFlags::READ),
        Err(Fs::Access)
    );
}

#[test]
fn a_read_of_a_length_that_is_not_a_chunk_multiple_returns_exactly_that_many() {
    let root = scratch("odd-length");
    std::fs::write(root.join("f"), vec![5u8; 20_000]).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let f = open(&mut host, "f", OpenFlags(0), DescriptorFlags::READ).unwrap();
    assert_eq!(read_at(&mut host, f, 10_000, 0), (vec![5u8; 10_000], false));
    assert_eq!(
        read_at(&mut host, f, 12_000, 5_000),
        (vec![5u8; 12_000], false)
    );
}

#[cfg(unix)]
#[test]
fn a_device_is_neither_a_file_nor_a_directory_nor_a_link() {
    let mut host = host_over(Path::new("/dev"), Access::ReadOnly);
    let null = open(&mut host, "null", OpenFlags(0), DescriptorFlags::READ).unwrap();
    // the type of an open file, and of a path looked up in the sandbox
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_get_type(null),
        Ok(DescriptorType::Unknown)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_stat_at(ROOT, FOLLOW, "null".into())
            .map(|s| s.type_),
        Ok(DescriptorType::Unknown)
    );
}

#[test]
fn a_directory_path_with_a_trailing_slash_or_dot_opens_the_directory() {
    let root = scratch("dir-suffix");
    std::fs::create_dir(root.join("sub")).unwrap();
    let mut host = host_over(&root, Access::ReadOnly);
    let dir = OpenFlags::DIRECTORY;
    for path in ["sub", "sub/", "sub/."] {
        let d = open(&mut host, path, dir, DescriptorFlags::READ).unwrap();
        assert_eq!(
            host.wasi_filesystem_types_method_descriptor_get_type(d),
            Ok(DescriptorType::Directory),
            "{path}"
        );
    }
}

#[test]
fn a_write_to_a_dropped_file_stream_is_closed_not_failed() {
    let root = scratch("dropped-output");
    let mut host = host_over(&root, Access::ReadWrite);
    let f = create(&mut host, "f");
    let w = host
        .wasi_filesystem_types_method_descriptor_write_via_stream(f, 0)
        .unwrap();
    host.wasi_io_streams_resource_drop_output_stream(w);
    assert_eq!(
        host.wasi_io_streams_method_output_stream_write(w, b"x".to_vec()),
        Err(StreamError::Closed)
    );
}
