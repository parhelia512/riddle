use std::env;

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<_>>();
    let options = riddle_lsp::parse_args(&args).unwrap_or_else(|error| error.exit());
    riddle_lsp::serve(options).await;
}
