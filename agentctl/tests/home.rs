#![allow(clippy::expect_used)]

use crate::support;

use std::path::Path;

use agent::local::home::ControlPlaneHome;

use support::TempDirectory;

#[test]
fn configured_home_is_absolute_and_uses_fixed_socket_path() {
    let temporary = TempDirectory::new("home-path");
    let relative = temporary.path().file_name().expect("temporary name");
    let home = ControlPlaneHome::resolve(Some(Path::new(relative))).expect("home should resolve");

    assert!(home.path().is_absolute());
    assert_eq!(home.socket_path().file_name(), Some("agentd.sock".as_ref()));
}

/// The longest socket path the check accepts is the longest a socket can listen on.
#[cfg(unix)]
#[test]
fn socket_path_check_matches_what_a_socket_accepts() {
    let temporary = TempDirectory::new("socket-limit");
    let home = |length: usize| {
        ControlPlaneHome::resolve(Some(&temporary.path().join("a".repeat(length)))).expect("home should resolve")
    };
    let longest = (1..200)
        .map(home)
        .take_while(|home| home.check_socket_path().is_ok())
        .last()
        .expect("a short home passes the check");
    let too_long = home(longest.path().file_name().expect("home name").len() + 1);
    too_long.check_socket_path().expect_err("one byte more fails the check");

    std::fs::create_dir_all(longest.socket_path().parent().expect("socket directory")).expect("socket directory");
    std::os::unix::net::UnixListener::bind(longest.socket_path()).expect("the longest accepted path can listen");
    std::os::unix::net::UnixListener::bind(too_long.socket_path()).expect_err("one byte more cannot");
}

#[test]
fn only_one_daemon_can_lock_a_home() {
    let temporary = TempDirectory::new("home-lock");
    let home = ControlPlaneHome::resolve(Some(temporary.path())).expect("home should resolve");
    let first = home.acquire_lock().expect("first lock should succeed");

    let second = home.acquire_lock().expect_err("second lock should fail");
    assert!(second.to_string().contains("already locked"));

    drop(first);
    home.acquire_lock().expect("lock should be released when dropped");
}

#[cfg(unix)]
#[test]
fn home_and_lock_file_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = TempDirectory::new("home-mode");
    let home = ControlPlaneHome::resolve(Some(temporary.path())).expect("home should resolve");
    let _lock = home.acquire_lock().expect("lock should succeed");

    let directory_mode = std::fs::metadata(home.path())
        .expect("home metadata")
        .permissions()
        .mode()
        & 0o777;
    let file_mode = std::fs::metadata(home.path().join("agentd.lock"))
        .expect("lock metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}
