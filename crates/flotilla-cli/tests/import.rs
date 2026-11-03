//! `flotilla import` is a move: the database left behind says where it went, and neither
//! `flotilla serve` nor a command that drives the engine starts on it again. Reading still works.

use flotilla_core::store::Store;

fn flotilla(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_flotilla"))
        .args(args)
        .env("FLOTILLA_DEV", "1")
        .output()
        .unwrap()
}

#[test]
fn an_import_leaves_the_source_saying_it_moved() {
    let dir = std::env::temp_dir().join(format!("flotilla-cli-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mac, server) = (dir.join("mac"), dir.join("server"));
    let mac_db = mac.join("flotilla.db");
    {
        let store = Store::open(&mac_db).unwrap();
        store
            .kv_set(flotilla_core::owner::SERVER_ID_KEY, "srv_mac")
            .unwrap();
    }
    let out = flotilla(&[
        "import",
        mac_db.to_str().unwrap(),
        "--data-dir",
        server.to_str().unwrap(),
    ]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("now says it moved here"), "{said}");
    let moved = flotilla_core::owner::moved(&mac_db).unwrap();
    assert_eq!(
        Some(moved.server.as_str()),
        flotilla_core::owner::recorded_id(&server.join("flotilla.db")).as_deref()
    );
    // The source's lock was let go.
    assert_eq!(flotilla_core::lock::holder(&mac_db), None);

    // No server on it, and no command that drives the engine; reading still works.
    let serve = flotilla(&[
        "serve",
        "--offline",
        "--port",
        "0",
        "--data-dir",
        mac.to_str().unwrap(),
    ]);
    let e = String::from_utf8_lossy(&serve.stderr);
    assert!(
        !serve.status.success() && e.contains("moved to") && e.contains(&moved.server),
        "{e}"
    );
    let archive = flotilla(&["--db", mac_db.to_str().unwrap(), "archive", "t1"]);
    let e = String::from_utf8_lossy(&archive.stderr);
    assert!(!archive.status.success() && e.contains("moved to"), "{e}");
    assert!(
        flotilla(&["--db", mac_db.to_str().unwrap(), "threads"])
            .status
            .success()
    );
    // The server's own copy didn't move anywhere.
    assert_eq!(
        flotilla_core::owner::moved(&server.join("flotilla.db")),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Beside another process that has the database without a socket (an older app), `threads`,
/// `show` and `search` only read: nothing is migrated or rebuilt, and a database at another
/// version is refused rather than changed under it.
#[test]
fn reading_beside_another_process_changes_nothing() {
    let dir = std::env::temp_dir().join(format!("flotilla-cli-read-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = dir.join("flotilla.db");
    let store = Store::open(&db).unwrap();
    let thread = flotilla_core::store::Thread {
        id: "t1".into(),
        environment_id: None,
        environment_name: Some("base".into()),
        title: "Fix the retry".into(),
        sandbox_id: None,
        conversation_id: None,
        provider: "claude-code".into(),
        model: "haiku".into(),
        effort: None,
        fast: false,
        state: flotilla_core::store::ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: flotilla_core::util::now_iso(),
        updated_at: flotilla_core::util::now_iso(),
        branch: None,
        repo_project_id: None,
        base_sha: None,
    };
    store.insert_thread(&thread).unwrap();
    let _app =
        flotilla_core::lock::Lock::take(&db, &flotilla_core::lock::Holder::new("app")).unwrap();
    let db_arg = db.to_str().unwrap();
    for args in [vec!["threads"], vec!["show", "t1"], vec!["search", "retry"]] {
        let out = flotilla(&[&["--db", db_arg][..], &args[..]].concat());
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Fix the retry"),
            "{args:?}"
        );
    }
    // Its folder, named as the data directory, is the same database; naming the database as
    // well is refused (they'd be two places), and nothing is made.
    let out = flotilla(&["threads", "--data-dir", dir.to_str().unwrap()]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("Fix the retry"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let elsewhere = dir.join("elsewhere");
    let out = flotilla(&[
        "--db",
        db_arg,
        "threads",
        "--data-dir",
        elsewhere.to_str().unwrap(),
    ]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not both"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!elsewhere.exists());
    // Its messages at another version: refused, and not rebuilt.
    store.kv_set("messages_version", "0").unwrap();
    let out = flotilla(&["--db", db_arg, "threads"]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("another version"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        store.kv_get("messages_version").unwrap().as_deref(),
        Some("0")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
