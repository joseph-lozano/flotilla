//! `flotilla serve` refuses to listen beyond this machine unless told `--insecure`, before it
//! touches anything.

#[test]
fn a_bind_beyond_this_machine_needs_insecure() {
    let dir = std::env::temp_dir().join(format!("flotilla-cli-serve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for host in ["0.0.0.0", "192.168.1.5", "::"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_flotilla"))
            .args(["serve", "--host", host, "--port", "0", "--data-dir"])
            .arg(&dir)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{host}");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("--insecure"), "{host}: {said}");
    }
    // Nothing was made.
    assert!(!dir.exists());
}
