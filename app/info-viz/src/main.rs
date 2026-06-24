mod app;
mod edit;
mod html;
mod http;
mod render;
mod web;

use std::{env, fs, path::PathBuf};

use app::AppState;
use http::Server;

const DEFAULT_ADDR: &str = "127.0.0.1:7878";
const SAMPLE_SOURCE: &str = r#"mod math {
    struct Number {
        value: i32,
    }

    impl Number {
        fun new(value: i32) -> Number {
            Number{value}
        }
    }
}

use crate::math::Number as Num;

fun main(x: i32) -> i32 {
    let n = Num::new(x);
    n.value
}

fun broken(flag: bool) -> i32 {
    let value: bool = 1;
    if flag { value } else { 0 }
}
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::parse(env::args().skip(1).collect())?;
    if config.help {
        Config::print_help();
        return Ok(());
    }

    let source = match &config.source {
        Some(path) => fs::read_to_string(path)?,
        None => SAMPLE_SOURCE.to_string(),
    };

    if let Some(path) = &config.source {
        println!("loaded {}", path.display());
    }

    Server::new(config.addr, AppState::new(source)).serve()?;
    Ok(())
}

struct Config {
    addr: String,
    source: Option<PathBuf>,
    help: bool,
}

impl Config {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut addr = DEFAULT_ADDR.to_string();
        let mut source = None;
        let mut help = false;
        let mut i = 0;

        while i < args.len() {
            match args[i].as_str() {
                "-h" | "--help" => {
                    help = true;
                    i += 1;
                }
                "--addr" => {
                    let Some(value) = args.get(i + 1) else {
                        return Err("--addr requires a value".into());
                    };
                    addr = value.clone();
                    i += 2;
                }
                arg if arg.starts_with("--addr=") => {
                    addr = arg["--addr=".len()..].to_string();
                    i += 1;
                }
                arg if arg.starts_with('-') => {
                    return Err(format!("unknown option `{arg}`"));
                }
                path => {
                    if source.is_some() {
                        return Err("only one source path is supported".into());
                    }
                    source = Some(PathBuf::from(path));
                    i += 1;
                }
            }
        }

        Ok(Self { addr, source, help })
    }

    fn print_help() {
        println!(
            "Usage: scope-graph-viz [--addr 127.0.0.1:7878] [source.rid]\n\
             Starts a local web UI with a live editor and Riddle semantic message visualization."
        );
    }
}
