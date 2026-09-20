use std::net::SocketAddr;

use acoustic_review::http;

fn main() {
    let mut listen: SocketAddr = "127.0.0.1:5322".parse().unwrap();
    let mut db_path = "acoustic-review.db".to_string();
    let mut seed = true;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                listen = args
                    .next()
                    .expect("--listen needs an address")
                    .parse()
                    .expect("valid socket address");
            }
            "--db" => db_path = args.next().expect("--db needs a path"),
            "--no-seed" => seed = false,
            "--help" | "-h" => {
                println!("acoustic-review [--listen 127.0.0.1:5322] [--db PATH] [--no-seed]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    http::listen(listen, &db_path, seed);
}
