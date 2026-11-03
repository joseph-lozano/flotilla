//! Files through a real server, as a paired device sends them: an attachment uploaded with the
//! device's token then sent by its blob id, and a skill packed, uploaded and given to every
//! agent. Neither goes as a path, and each is noted as from that device. The device can't set
//! the server's boat key.

use flotilla_client::{Client, Info, Target};
use flotilla_core::devices::CODE_TTL;
use flotilla_core::store::{Store, Thread, ThreadState};
use flotilla_engine::Cmd;
use flotilla_engine::host::{Mode, Role};
use flotilla_proto::{
    Attached, ClientMsg, Kind, Reply, ReqId, Request, Secret, ServerMsg, View, ViewKey,
};
use flotilla_server::{Config, Listen, Server};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

struct Running {
    rt: tokio::runtime::Runtime,
    dir: PathBuf,
    db: PathBuf,
    url: String,
    cmds: Arc<Mutex<Vec<Cmd>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(done) = self.done.take() {
            let _ = self.rt.block_on(done);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(name: &str) -> Running {
    let dir = std::env::temp_dir().join(format!("flotilla-uploads-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let cmds: Arc<Mutex<Vec<Cmd>>> = Arc::default();
    let record = cmds.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listen = vec![
        Listen::tcp("127.0.0.1", 0, false).unwrap(),
        Listen::Unix(dir.join("run").join("s.sock")),
    ];
    let mode = Mode::Fake(Arc::new(move |c| record.lock().unwrap().push(c)));
    let server = rt
        .block_on(Server::bind(Config {
            db: db.clone(),
            listen,
            mode,
            role: Role::Server,
        }))
        .unwrap();
    let url = format!("http://{}", server.tcp().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        rt,
        dir,
        db,
        url,
        cmds,
        stop: Some(stop),
        done: Some(done),
    }
}

/// A connected device, and what the server sends it.
struct Device {
    id: String,
    token: Secret,
    client: Client,
    rx: mpsc::Receiver<ServerMsg>,
    next: ReqId,
}

impl Device {
    fn pair(s: &Running, name: &str) -> Device {
        let code = Store::open(&s.db)
            .unwrap()
            .mint_code(CODE_TTL, "cli", flotilla_core::util::now_ms())
            .unwrap()
            .code;
        let p = flotilla_client::pair(&s.url, &code, name, "macos").unwrap();
        let (tx, rx) = mpsc::channel();
        let client = Client::connect(
            Target::parse(&s.url).unwrap(),
            Info {
                app: "test".into(),
                version: "0".into(),
                token: Some(p.token.clone()),
                scope: None,
            },
            move |m| tx.send(m).is_ok(),
        );
        Device {
            id: p.device,
            token: p.token,
            client,
            rx,
            next: 1,
        }
    }

    fn ask(&mut self, req: Request) -> Result<Reply, String> {
        let id = self.next;
        self.next += 1;
        self.client.send(ClientMsg::Req { id, req });
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match self
                .rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("an answer")
            {
                ServerMsg::Res { id: i, result } if i == id => return result,
                _ => continue,
            }
        }
    }

    fn settings(
        &mut self,
        until: impl Fn(&flotilla_proto::Settings) -> bool,
    ) -> flotilla_proto::Settings {
        self.client.send(ClientMsg::Sub {
            view: ViewKey::Settings,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let ServerMsg::Snap { view } = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("the settings")
                && let View::Settings(st) = *view
                && until(&st)
            {
                return st;
            }
        }
    }
}

fn thread(id: &str) -> Thread {
    Thread {
        id: id.into(),
        environment_id: Some("e1".into()),
        environment_name: Some("base".into()),
        title: "Look at a file".into(),
        sandbox_id: Some("bx_1".into()),
        conversation_id: Some("c1".into()),
        provider: "claude-code".into(),
        model: "haiku".into(),
        effort: None,
        fast: false,
        state: ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: "2026-10-31T10:00:00.000Z".into(),
        updated_at: "2026-10-31T10:00:00.000Z".into(),
        repo_project_id: None,
        branch: None,
        base_sha: None,
    }
}

#[test]
fn an_attachment_goes_up_then_by_its_blob_id() {
    let s = start("attach");
    Store::open(&s.db)
        .unwrap()
        .insert_thread(&thread("t1"))
        .unwrap();
    let mut d = Device::pair(&s, "My MacBook");
    let target = Target::parse(&s.url).unwrap();
    // Picked on the device: uploaded as it is there, with the device's token.
    let file = s.dir.join("notes.txt");
    std::fs::write(&file, "the codeword is PLUM\n").unwrap();
    let picked = flotilla_core::attach::from_path(&file).unwrap();
    assert!(
        flotilla_core::attach::upload(std::slice::from_ref(&picked), |b| flotilla_client::upload(
            &target, None, b
        ))
        .unwrap_err()
        .contains("Couldn't upload")
    );
    let up = flotilla_core::attach::upload(&[picked], |b| {
        flotilla_client::upload(&target, Some(&d.token), b)
    })
    .unwrap();
    std::fs::remove_file(&file).unwrap();
    // The stand-in engine never answers a send: wait for it to be asked.
    d.client.send(ClientMsg::Req {
        id: 100,
        req: Request::Send {
            thread: "t1".into(),
            text: "cat it".into(),
            files: up.clone(),
            local: "local-1".into(),
        },
    });
    let until = Instant::now() + Duration::from_secs(10);
    let sent = loop {
        let asked = s.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::Act {
                action: flotilla_engine::Action::Send { files, .. },
                ..
            } => Some(files.clone()),
            _ => None,
        });
        match asked {
            Some(files) => break files,
            None if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            None => panic!("the engine wasn't asked to send"),
        }
    };
    assert_eq!(sent[0].from.as_deref(), Some(d.id.as_str()));
    // The engine sends what was uploaded, though the file's gone from the device.
    let blobs = flotilla_core::blobs::Blobs::beside(&s.db);
    let boat = flotilla_core::attach::load(&sent, &blobs).unwrap();
    assert_eq!(boat[0].filename, "notes.txt");
    assert_eq!(boat[0].base64_data, "dGhlIGNvZGV3b3JkIGlzIFBMVU0K");
    // A path instead of an upload is refused before anything's read.
    let path = Attached {
        path: "/etc/passwd".into(),
        name: "passwd".into(),
        size: 1,
        kind: Kind::Text,
        blob: String::new(),
        from: None,
    };
    let r = d.ask(Request::Send {
        thread: "t1".into(),
        text: "x".into(),
        files: vec![path],
        local: "local-2".into(),
    });
    assert_eq!(r, Err("passwd wasn't uploaded".into()));
    // And the device can't set the server's boat key.
    let r = d.ask(Request::SetBoatKey(Secret("boat_nope".into())));
    assert!(r.unwrap_err().contains("flotilla setup"));
}

#[test]
fn a_skill_goes_up_packed_and_says_where_it_came_from() {
    let s = start("skill");
    let mut d = Device::pair(&s, "My MacBook");
    let target = Target::parse(&s.url).unwrap();
    let folder = s.dir.join("skills").join("plum");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        "---\ndescription: Say the codeword\n---\nSay PLUM.\n",
    )
    .unwrap();
    let skill = flotilla_core::skills::read(&folder).unwrap();
    let packed = flotilla_core::skills::pack(std::slice::from_ref(&skill)).unwrap();
    let blob = flotilla_client::upload(&target, Some(&d.token), &packed).unwrap();
    assert_eq!(
        d.ask(Request::PutSkill {
            skill: skill.clone(),
            blob: blob.id.clone()
        }),
        Ok(Reply::Done)
    );
    let st = d.settings(|st| !st.skills.is_empty());
    let kept = &st.skills[0];
    assert_eq!(
        (
            kept.name.as_str(),
            kept.description.as_str(),
            kept.blob.as_str()
        ),
        ("plum", "Say the codeword", blob.id.as_str())
    );
    assert_eq!(
        (
            kept.from.as_deref(),
            kept.from_name.as_str(),
            kept.folder.as_path()
        ),
        (Some(d.id.as_str()), "My MacBook", folder.as_path())
    );
    // What the server provisions VMs from unpacks to the folder.
    let bytes = flotilla_core::blobs::Blobs::beside(&s.db)
        .read(&kept.blob)
        .unwrap();
    let out = s.dir.join("vm");
    std::fs::create_dir_all(&out).unwrap();
    let mut tar = std::process::Command::new("tar")
        .args(["-xzf", "-", "-C"])
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(tar.stdin.as_mut().unwrap(), &bytes).unwrap();
    assert!(tar.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(out.join("plum/SKILL.md")).unwrap(),
        "---\ndescription: Say the codeword\n---\nSay PLUM.\n"
    );
    // Update: packed again and put in its place.
    std::fs::write(
        folder.join("SKILL.md"),
        "---\ndescription: Say the new codeword\n---\nSay PEAR.\n",
    )
    .unwrap();
    let skill = flotilla_core::skills::read(&folder).unwrap();
    let again = flotilla_client::upload(
        &target,
        Some(&d.token),
        &flotilla_core::skills::pack(std::slice::from_ref(&skill)).unwrap(),
    )
    .unwrap();
    assert_ne!(again.id, blob.id);
    assert_eq!(
        d.ask(Request::PutSkill {
            skill,
            blob: again.id.clone()
        }),
        Ok(Reply::Done)
    );
    let kept = flotilla_core::skills::chosen(&Store::open(&s.db).unwrap());
    assert_eq!(
        (
            kept.len(),
            kept[0].blob.as_str(),
            kept[0].description.as_str()
        ),
        (1, again.id.as_str(), "Say the new codeword")
    );
}
