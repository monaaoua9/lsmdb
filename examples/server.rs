use lsmdb::{Db, Options};
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
};

// line protocol: GET k | SET k v | DEL k | INCR k [n] | SCAN prefix [limit] | STATS | QUIT
fn handle(db: &Db, s: TcpStream) -> std::io::Result<()> {
    let mut out = s.try_clone()?;
    for line in BufReader::new(s).lines() {
        let line = line?;
        let mut p = line.splitn(3, ' ');
        let reply = match (p.next().unwrap_or("").to_ascii_uppercase().as_str(), p.next(), p.next()) {
            ("GET", Some(k), _) => match db.get(k.as_bytes()) {
                Ok(Some(v)) => format!("+{}", String::from_utf8_lossy(&v)),
                Ok(None) => "-nil".into(),
                Err(e) => format!("-{e}"),
            },
            ("SET", Some(k), Some(v)) => db.put(k, v).map_or_else(|e| format!("-{e}"), |_| "+ok".into()),
            ("DEL", Some(k), _) => db.delete(k).map_or_else(|e| format!("-{e}"), |_| "+ok".into()),
            ("INCR", Some(k), n) => {
                let by = n.and_then(|n| n.parse().ok()).unwrap_or(1);
                db.increment(k.as_bytes(), by).map_or_else(|e| format!("-{e}"), |v| format!(":{v}"))
            }
            ("SCAN", prefix, limit) => {
                let limit = limit.and_then(|n| n.parse().ok()).unwrap_or(100);
                match db.prefix(prefix.unwrap_or("").as_bytes()) {
                    Ok(it) => it
                        .take(limit)
                        .map(|r| {
                            r.map(|(k, v)| {
                                format!("{}={}", String::from_utf8_lossy(&k), String::from_utf8_lossy(&v))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_or_else(|e| format!("-{e}"), |v| format!("+{}", v.join(","))),
                    Err(e) => format!("-{e}"),
                }
            }
            ("STATS", ..) => format!("+{:?}", db.stats()),
            ("QUIT", ..) => return Ok(()),
            _ => "-bad command".into(),
        };
        writeln!(out, "{reply}")?;
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "server-data".into());
    let addr = std::env::args().nth(2).unwrap_or_else(|| "127.0.0.1:7878".into());
    let db = Arc::new(Db::open(&dir, Options::default()).expect("open"));
    let l = TcpListener::bind(&addr)?;
    eprintln!("serving {dir} on {addr}");
    for s in l.incoming().flatten() {
        let db = db.clone();
        thread::spawn(move || {
            let _ = handle(&db, s);
        });
    }
    Ok(())
}
