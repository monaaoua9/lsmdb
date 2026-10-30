use lsmdb::{Db, Options};
use std::{env, process};

fn usage() -> ! {
    eprintln!(
        "usage: lsmcli <dir> <command> [args]\n\n\
         get <key>                 print a value\n\
         put <key> <value>\n\
         del <key>\n\
         scan [prefix] [limit]     list keys, in order\n\
         rscan [prefix] [limit]    same, descending\n\
         count                     number of live keys\n\
         stats                     counters and level sizes\n\
         tables                    live table files\n\
         compact                   flush and compact everything\n\
         verify                    full integrity scan\n\
         checkpoint <dest>         hard-linked backup"
    );
    process::exit(2);
}

fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn main() -> lsmdb::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 2 {
        usage();
    }
    let read_only =
        matches!(args[1].as_str(), "get" | "scan" | "rscan" | "count" | "stats" | "tables" | "verify");
    let opts = Options { read_only, create_if_missing: !read_only, ..Options::default() };
    let db = Db::open(&args[0], opts)?;
    let rest = &args[2..];

    match (args[1].as_str(), rest) {
        ("get", [k]) => match db.get(k.as_bytes())? {
            Some(v) => println!("{}", show(&v)),
            None => {
                eprintln!("not found");
                process::exit(1);
            }
        },
        ("put", [k, v]) => db.put(k.as_bytes(), v.as_bytes())?,
        ("del", [k]) => db.delete(k.as_bytes())?,
        (cmd @ ("scan" | "rscan"), r) => {
            let prefix = r.first().map(|p| p.as_bytes()).unwrap_or(b"");
            let limit = r.get(1).and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
            let it = db.prefix(prefix)?;
            if cmd == "scan" {
                for kv in it.take(limit) {
                    let (k, v) = kv?;
                    println!("{}\t{}", show(&k), show(&v));
                }
            } else {
                for kv in it.rev().take(limit) {
                    let (k, v) = kv?;
                    println!("{}\t{}", show(&k), show(&v));
                }
            }
        }
        ("count", []) => println!("{}", db.iter()?.count()),
        ("stats", []) => {
            let s = db.stats();
            println!("{s:#?}");
            println!("write amplification: {:.2}", s.write_amplification());
        }
        ("tables", []) => {
            for t in db.table_info() {
                println!(
                    "L{} #{:06} {:>10} B  {} .. {}",
                    t.level,
                    t.number,
                    t.size,
                    show(&t.smallest_key),
                    show(&t.largest_key)
                );
            }
        }
        ("compact", []) => db.compact_all()?,
        ("verify", []) => {
            let r = db.verify_integrity()?;
            println!("ok: {} tables, {} entries, {} bytes", r.tables, r.entries, r.bytes);
        }
        ("checkpoint", [dest]) => db.checkpoint(dest)?,
        _ => usage(),
    }
    Ok(())
}
