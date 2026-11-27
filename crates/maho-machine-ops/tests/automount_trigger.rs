#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_machine_ops::{
    MountEntry, MountView, create_automount_trigger_lookup, parse_darwin_mount_table,
    parse_linux_mount_info, unmounted_automount_path,
};

fn view_of(mounts: Vec<MountEntry>, case_insensitive: bool) -> MountView {
    MountView::new(mounts, case_insensitive)
}

fn sample_view(case_insensitive: bool) -> MountView {
    MountView {
        mounts: vec![
            MountEntry::new("/", "apfs"),
            MountEntry::new("/System/Volumes/Data/home", "autofs"),
            MountEntry::new("/System/Volumes/Data/home/alice", "nfs"),
        ],
        aliases: BTreeMap::from([("/home".into(), "/System/Volumes/Data/home".into())]),
        case_insensitive,
    }
}

#[test]
fn reads_mount_points_and_types_from_proc_self_mountinfo_unescaping_octal() {
    let text = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
        36 25 0:31 / /proc/sys/fs/binfmt_misc rw,relatime shared:13 - autofs systemd-1 rw,fd=32,direct\n\
        90 22 0:50 / /mnt/my\\040share rw,relatime - nfs4 server:/export rw";
    let expected = vec![
        MountEntry::new("/", "ext4"),
        MountEntry::new("/proc/sys/fs/binfmt_misc", "autofs"),
        MountEntry::new("/mnt/my share", "nfs4"),
    ];
    assert_eq!(parse_linux_mount_info(text), expected);
}

#[test]
fn reads_mount_points_and_types_from_macos_mount_output() {
    let text = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
        map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)\n\
        //user@nas/My Share on /Users/me/NAS/My Share (smbfs, nodev, nosuid, mounted by me)";
    let expected = vec![
        MountEntry::new("/", "apfs"),
        MountEntry::new("/System/Volumes/Data/home", "autofs"),
        MountEntry::new("/Users/me/NAS/My Share", "smbfs"),
    ];
    assert_eq!(parse_darwin_mount_table(text), expected);
}

#[test]
fn maps_a_path_through_the_root_alias_onto_the_trigger_it_reaches() {
    let view = sample_view(false);
    let sub = unmounted_automount_path("/home/someone/work/x", &view);
    assert_eq!(
        sub.as_deref(),
        Some("/System/Volumes/Data/home/someone/work/x")
    );
    let root = unmounted_automount_path("/home", &view);
    assert_eq!(root.as_deref(), Some("/System/Volumes/Data/home"));
}

#[test]
fn leaves_a_filesystem_already_mounted_below_the_trigger_and_every_other_path_to_normal_resolution()
{
    let view = sample_view(false);
    assert_eq!(
        unmounted_automount_path("/home/alice/notes.md", &view),
        None
    );
    assert_eq!(unmounted_automount_path("/homework/x", &view), None);
    assert_eq!(unmounted_automount_path("/Users/me/x", &view), None);
}

#[test]
fn treats_a_filesystem_stacked_on_the_triggers_own_mount_point_as_mounted_whichever_entry_the_table_lists_first()
 {
    let autofs = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
        40 22 0:40 / /mnt/nas rw,relatime shared:20 - autofs systemd-1 rw,fd=45,direct\n\
        41 22 0:41 / /boot rw,relatime shared:21 - autofs systemd-1 rw,fd=46,direct";
    let mounted = "90 40 0:50 / /mnt/nas rw,relatime shared:30 - nfs4 server:/export rw\n\
        91 41 259:1 / /boot rw,relatime shared:31 - vfat /dev/nvme0n1p1 rw";
    let linux = |text: &str| view_of(parse_linux_mount_info(text), false);
    let both = format!("{autofs}\n{mounted}");
    let rev = format!("{mounted}\n{autofs}");

    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(autofs)).as_deref(),
        Some("/mnt/nas/link/secret")
    );
    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(&both)),
        None
    );
    assert_eq!(unmounted_automount_path("/boot/efi/x", &linux(&both)), None);
    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(&rev)),
        None
    );
}

#[test]
fn treats_a_macos_direct_map_share_mounted_over_its_autofs_entry_as_mounted() {
    let trigger = "map auto_nas on /Volumes/nas (autofs, automounted, nobrowse)";
    let share = "server:/export on /Volumes/nas (nfs, nodev, nosuid, automounted, nobrowse)";
    let darwin = |text: &str| view_of(parse_darwin_mount_table(text), true);

    assert_eq!(
        unmounted_automount_path("/Volumes/nas/link/secret", &darwin(trigger)).as_deref(),
        Some("/Volumes/nas/link/secret")
    );
    let stacked = format!("{trigger}\n{share}");
    assert_eq!(
        unmounted_automount_path("/Volumes/nas/link/secret", &darwin(&stacked)),
        None
    );
}

#[test]
fn matches_mount_points_and_root_aliases_regardless_of_case_on_a_case_insensitive_volume() {
    let view = sample_view(false);
    let folded = sample_view(true);

    assert_eq!(
        unmounted_automount_path("/HOME/someone/x", &folded).as_deref(),
        Some("/System/Volumes/Data/home/someone/x")
    );
    assert_eq!(
        unmounted_automount_path("/System/Volumes/Data/HOME/someone/x", &folded).as_deref(),
        Some("/System/Volumes/Data/home/someone/x")
    );
    assert_eq!(
        unmounted_automount_path("/home/ALICE/notes.md", &folded),
        None
    );
    assert_eq!(unmounted_automount_path("/HOME/someone/x", &view), None);
}

#[tokio::test]
async fn re_reads_the_mount_view_before_a_trigger_answer_so_a_home_that_mounted_since_is_resolved_normally()
 {
    let unmounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
        ],
        false,
    );
    let mounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
            MountEntry::new("/home/alice", "nfs"),
        ],
        false,
    );
    let views = Arc::new(vec![unmounted, mounted]);
    let reads = Arc::new(AtomicUsize::new(0));
    let (rv, rr) = (Arc::clone(&views), Arc::clone(&reads));
    let lookup = create_automount_trigger_lookup(move || {
        let idx = rr.fetch_add(1, Ordering::SeqCst);
        let view = rv[idx.min(rv.len() - 1)].clone();
        async move { view }
    });

    assert_eq!(
        lookup.lookup("/home/alice/link/x").await.as_deref(),
        Some("/home/alice/link/x")
    );
    assert_eq!(lookup.lookup("/home/alice/link/x").await, None);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn classifies_other_paths_from_the_cached_view_and_shares_one_re_read_among_concurrent_trigger_answers()
 {
    let unmounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
        ],
        false,
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let rr = Arc::clone(&reads);
    let lookup = create_automount_trigger_lookup(move || {
        rr.fetch_add(1, Ordering::SeqCst);
        let view = unmounted.clone();
        async move { view }
    });
    assert_eq!(lookup.lookup("/srv/x").await, None);
    assert_eq!(lookup.lookup("/srv/y").await, None);
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let l = lookup.clone();
            tokio::spawn(l.lookup(format!("/home/user-{i}/x")))
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.await.unwrap(), Some(format!("/home/user-{i}/x")));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}
