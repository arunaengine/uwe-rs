//! The iRODS provider against a local fake server of the protocol subset that uwe uses: TLS
//! negotiation, native and PAM sign-in, stat, paged queries, reads, writes and collections.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine;
use config_file::ConfigFile;
use md5::{Digest, Md5};
use serde_json::{Value, json};
use storage::Storage;

const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PAM_SESSION: &str = "short-lived";

/// What the fake holds: data objects, collections, and how many connections it took.
#[derive(Default)]
struct Zone {
    objects: HashMap<String, Vec<u8>>,
    collections: BTreeSet<String>,
    connections: usize,
}

struct Server {
    port: u16,
    zone: Arc<Mutex<Zone>>,
}

/// A fake with the password `rods`, the PAM password `pam-pass`, and this TLS policy.
fn server(policy: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("an address").port();
    let zone = Arc::new(Mutex::new(Zone::default()));
    zone.lock()
        .expect("the zone")
        .collections
        .extend(["/tempZone".into(), "/tempZone/home".into()]);
    let shared = zone.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            shared.lock().expect("the zone").connections += 1;
            let zone = shared.clone();
            std::thread::spawn(move || serve(stream, &zone, policy));
        }
    });
    Server { port, zone }
}

struct Frame {
    kind: String,
    body: String,
    bytes: Vec<u8>,
    info: i32,
}

fn tag(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let start = xml
        .find(&open)
        .map_or(xml.len(), |start| start + open.len());
    let end = xml[start..]
        .find(&format!("</{name}>"))
        .map_or(start, |end| start + end);
    xml[start..end].replace("&apos;", "'").replace("&amp;", "&")
}

fn receive(stream: &mut TcpStream) -> Option<Frame> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).ok()?;
    let mut header = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut header).ok()?;
    let header = String::from_utf8(header).ok()?;
    let mut part = |name| {
        let mut data = vec![0; tag(&header, name).parse().ok()?];
        stream.read_exact(&mut data).ok()?;
        Some(data)
    };
    let (body, _, bytes) = (part("msgLen")?, part("errorLen")?, part("bsLen")?);
    Some(Frame {
        kind: tag(&header, "type"),
        body: String::from_utf8(body).ok()?,
        bytes,
        info: tag(&header, "intInfo").parse().ok()?,
    })
}

fn send(stream: &mut TcpStream, kind: &str, body: &str, bytes: &[u8], info: i32) {
    let header = format!(
        "<MsgHeader_PI><type>{kind}</type><msgLen>{}</msgLen><errorLen>0</errorLen>\
         <bsLen>{}</bsLen><intInfo>{info}</intInfo></MsgHeader_PI>",
        body.len(),
        bytes.len()
    );
    let mut message = (header.len() as u32).to_be_bytes().to_vec();
    message.extend_from_slice(header.as_bytes());
    message.extend_from_slice(body.as_bytes());
    message.extend_from_slice(bytes);
    let _ = stream.write_all(&message);
}

fn json_reply(value: Value) -> String {
    let text = base64::engine::general_purpose::STANDARD.encode(value.to_string());
    format!(
        "<BinBytesBuf_PI><buflen>{}</buflen><buf>{text}</buf></BinBytesBuf_PI>",
        text.len()
    )
}

fn digest(password: &str) -> String {
    let mut buffer = [0; 114];
    buffer[..64].copy_from_slice(CHALLENGE.as_bytes());
    buffer[64..64 + password.len()].copy_from_slice(password.as_bytes());
    let hash: Vec<u8> = (Md5::digest(buffer).iter())
        .map(|byte| if *byte == 0 { 1 } else { *byte })
        .collect();
    base64::engine::general_purpose::STANDARD.encode(hash)
}

/// One connection: the startup, the negotiation, then API requests until it closes.
fn serve(mut stream: TcpStream, zone: &Mutex<Zone>, policy: &str) {
    let Some(startup) = receive(&mut stream) else {
        return;
    };
    assert_eq!(startup.kind, "RODS_CONNECT");
    assert!(startup.body.contains("request_server_negotiation"));
    let offer = format!("<CS_NEG_PI><status>1</status><result>{policy}</result></CS_NEG_PI>");
    send(&mut stream, "RODS_CS_NEG_T", &offer, &[], 0);
    match receive(&mut stream) {
        Some(answer) if answer.body.contains("cs_neg_result_kw=CS_NEG_USE_TCP;") => {}
        _ => return,
    }
    let version = "<Version_PI><status>0</status><relVersion>rods4.3.2</relVersion>\
                   <apiVersion>d</apiVersion><reconnPort>0</reconnPort><reconnAddr></reconnAddr>\
                   <cookie>400</cookie></Version_PI>";
    send(&mut stream, "RODS_VERSION", version, &[], 0);
    let mut password = "rods".to_owned();
    let mut signed_in = false;
    let mut open: HashMap<i32, (String, usize)> = HashMap::new();
    let mut pages: HashMap<String, usize> = HashMap::new();
    while let Some(request) = receive(&mut stream) {
        if request.kind == "RODS_DISCONNECT" {
            return;
        }
        let (code, body, bytes) = match request.info {
            110000 => {
                let buffer = tag(&request.body, "buf");
                let decoded = base64::engine::general_purpose::STANDARD.decode(buffer);
                let asked: Value = serde_json::from_slice(&decoded.expect("base64")).expect("JSON");
                assert_eq!(
                    (&asked["user_name"], &asked["zone_name"]),
                    (&json!("rods"), &json!("tempZone"))
                );
                match (asked["scheme"].as_str(), asked["next_operation"].as_str()) {
                    (Some("pam_password"), _) if asked["a_pw"] == "pam-pass" => {
                        password = PAM_SESSION.to_owned();
                        (
                            0,
                            json_reply(json!({ "request_result": PAM_SESSION })),
                            Vec::new(),
                        )
                    }
                    (Some("native"), Some("auth_agent_auth_request")) => (
                        0,
                        json_reply(json!({ "request_result": CHALLENGE })),
                        Vec::new(),
                    ),
                    (Some("native"), Some("auth_agent_auth_response"))
                        if asked["digest"] == digest(&password) =>
                    {
                        signed_in = true;
                        (0, json_reply(json!({})), Vec::new())
                    }
                    _ => (-826000, String::new(), Vec::new()),
                }
            }
            _ if !signed_in => (-827000, String::new(), Vec::new()),
            api => answer(
                api,
                &request,
                &mut zone.lock().expect("the zone"),
                &mut open,
                &mut pages,
            ),
        };
        send(&mut stream, "RODS_API_REPLY", &body, &bytes, code);
    }
}

/// The answer to an API request after sign-in: its status, its body and its bytes.
fn answer(
    api: i32,
    request: &Frame,
    zone: &mut Zone,
    open: &mut HashMap<i32, (String, usize)>,
    pages: &mut HashMap<String, usize>,
) -> (i32, String, Vec<u8>) {
    let path = tag(&request.body, "objPath");
    let descriptor: i32 = tag(&request.body, "l1descInx").parse().unwrap_or(0);
    match api {
        633 => match (zone.objects.get(&path), zone.collections.contains(&path)) {
            (Some(data), _) => {
                let stat = format!(
                    "<RodsObjStat_PI><objSize>{}</objSize><objType>1</objType>\
                     <modifyTime>01700000000</modifyTime></RodsObjStat_PI>",
                    data.len()
                );
                (0, stat, Vec::new())
            }
            (None, true) => (
                0,
                "<RodsObjStat_PI><objSize>0</objSize><objType>2</objType>\
                                 <modifyTime>01700000000</modifyTime></RodsObjStat_PI>"
                    .into(),
                Vec::new(),
            ),
            (None, false) => (-310000, String::new(), Vec::new()),
        },
        702 => {
            let select = tag(&tag(&request.body, "InxIvalPair_PI"), "inx");
            let condition = tag(&request.body, "InxValPair_PI");
            let value = tag(&condition, "svalue");
            let parent = value
                .trim_start_matches("= '")
                .trim_end_matches('\'')
                .to_owned();
            let rows: Vec<String> = match (select.as_str(), tag(&condition, "inx").as_str()) {
                ("501", "502") => (zone.collections.iter())
                    .filter(|path| path.rsplit_once('/').map(|(up, _)| up) == Some(parent.as_str()))
                    .cloned()
                    .collect(),
                ("403", "501") => {
                    let mut names: Vec<String> = (zone.objects.keys())
                        .filter_map(|path| path.strip_prefix(&format!("{parent}/")))
                        .filter(|name| !name.contains('/'))
                        .map(str::to_owned)
                        .collect();
                    names.sort();
                    names
                }
                other => panic!("unexpected query {other:?}"),
            };
            // Pages of two rows, so that listings must follow the continuation.
            let key = format!("{select} {parent}");
            let start = match tag(&request.body, "continueInx").as_str() {
                "0" => 0,
                _ => pages.get(&key).copied().unwrap_or(0),
            };
            if start >= rows.len() {
                return (-808000, String::new(), Vec::new());
            }
            let end = (start + 2).min(rows.len());
            pages.insert(key, end);
            let next = if end < rows.len() { 1 } else { 0 };
            let values: String = rows[start..end]
                .iter()
                .map(|row| format!("<value>{row}</value>"))
                .collect();
            let out = format!(
                "<GenQueryOut_PI><rowCnt>{}</rowCnt><attriCnt>1</attriCnt>\
                 <continueInx>{next}</continueInx><totalRowCount>0</totalRowCount>\
                 <SqlResult_PI><attriInx>{select}</attriInx><reslen>256</reslen>{values}\
                 </SqlResult_PI></GenQueryOut_PI>",
                end - start
            );
            (0, out, Vec::new())
        }
        681 => {
            let mut path = tag(&request.body, "collName");
            assert!(request.body.contains("<keyWord>recursiveOpr</keyWord>"));
            while path.len() > 1 {
                zone.collections.insert(path.clone());
                path = path.rsplit_once('/').map_or("", |(up, _)| up).to_owned();
            }
            (0, String::new(), Vec::new())
        }
        601 | 602 => {
            let parent = path.rsplit_once('/').map_or("", |(up, _)| up);
            match api {
                601 if zone.collections.contains(parent) => {
                    assert!(request.body.contains("<keyWord>forceFlag</keyWord>"));
                    zone.objects.insert(path.clone(), Vec::new());
                }
                601 => return (-814000, String::new(), Vec::new()),
                _ if !zone.objects.contains_key(&path) => {
                    return (-310000, String::new(), Vec::new());
                }
                _ => {}
            }
            let descriptor = 3 + open.len() as i32;
            open.insert(descriptor, (path, 0));
            (descriptor, String::new(), Vec::new())
        }
        675 => {
            let length: usize = tag(&request.body, "len").parse().unwrap_or(0);
            let (path, offset) = open.get_mut(&descriptor).expect("an open object");
            let data = &zone.objects[path.as_str()];
            let end = (*offset + length).min(data.len());
            let chunk = data[*offset..end].to_vec();
            *offset = end;
            (chunk.len() as i32, String::new(), chunk)
        }
        676 => {
            let (path, _) = open.get(&descriptor).expect("an open object");
            let object = zone.objects.get_mut(path.as_str()).expect("an object");
            object.extend_from_slice(&request.bytes);
            (request.bytes.len() as i32, String::new(), Vec::new())
        }
        673 => {
            open.remove(&descriptor).expect("an open object");
            (0, String::new(), Vec::new())
        }
        other => panic!("unexpected API {other}"),
    }
}

/// A provider for the fake with this TLS policy and more settings.
fn storage(server: &Server, policy: &str, extra: &str) -> Storage {
    let text = format!(
        "[storage.zone]\ntype = \"irods\"\nprefixes = [\"irods://\"]\nhost = \"127.0.0.1\"\n\
         port = {}\nuser = \"rods\"\nzone = \"tempZone\"\nnegotiation = \"{policy}\"\n{extra}",
        server.port
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

#[tokio::test]
async fn writes_data_objects() {
    let server = server("CS_NEG_REFUSE");
    let extra = "password = \"rods\"\nauthentication = \"native\"\n";
    let storage = storage(&server, "CS_NEG_DONT_CARE", extra);
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("out.txt");
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(&file, format!("Hello from {name}\n")).expect("a file");
        let target = format!("irods:///tempZone/home/rods/data/{name}");
        let written = storage.upload_with_receipt(&file, &target).await;
        assert_eq!(written.expect("an upload").size, 17);
    }
    let found = storage.stat("irods:///tempZone/home/rods/data/b.txt").await;
    let found = found.expect("a stat").expect("an object");
    assert_eq!((found.directory, found.size), (false, 17));
    assert_eq!(found.version.as_deref(), Some("1700000000"));
    let home = storage
        .list("irods://tempZone/home/rods")
        .await
        .expect("a listing");
    let names: Vec<(&str, bool)> = (home.iter())
        .map(|entry| (entry.name.as_str(), entry.directory))
        .collect();
    assert_eq!(names, [("data", true)]);
    let data = storage
        .list("irods:///tempZone/home/rods/data/")
        .await
        .expect("a listing");
    let names: Vec<&str> = data.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        ["a.txt", "b.txt", "c.txt"],
        "every page of the listing"
    );
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage
        .fetch("irods://tempZone/home/rods/data/c.txt", downloads.path())
        .await;
    let copy = copy.expect("a copy");
    assert_eq!(
        std::fs::read_to_string(copy).expect("the copy"),
        "Hello from c.txt\n"
    );
    let missing = storage.stat("irods:///tempZone/home/rods/none.txt").await;
    assert!(missing.expect("a stat").is_none());
    let error = storage.list("irods:///tempZone/home/rods/data/a.txt").await;
    assert!(error.is_err(), "a data object is not a collection");
    assert_eq!(
        server.zone.lock().expect("the zone").connections,
        1,
        "one session"
    );
}

#[tokio::test]
async fn pam_needs_tls() {
    let server = server("CS_NEG_DONT_CARE");
    let extra = "password = \"pam-pass\"\nauthentication = \"pam_password\"\n";
    let storage = storage(&server, "CS_NEG_REFUSE", extra);
    let error = storage
        .stat("irods:///tempZone/home")
        .await
        .expect_err("no PAM password over plain TCP");
    assert!(error.to_string().contains("needs TLS"), "{error}");
}

#[tokio::test]
async fn refuses_wrong_passwords() {
    let server = server("CS_NEG_REFUSE");
    let wrong = storage(&server, "CS_NEG_REFUSE", "password = \"wrong\"\n");
    let error = wrong
        .stat("irods:///tempZone/home")
        .await
        .expect_err("a refusal");
    let message = error.to_string();
    assert!(
        message.contains("Failed to initialize iRODS connection"),
        "{message}"
    );
    assert!(message.contains("Invalid user or password"), "{message}");
    assert!(!message.contains("wrong"), "{message}");
    let strict = storage(&server, "CS_NEG_REQUIRE", "password = \"rods\"\n");
    let error = strict
        .stat("irods:///tempZone/home")
        .await
        .expect_err("no agreement");
    assert!(error.to_string().contains("do not agree on TLS"), "{error}");
}
