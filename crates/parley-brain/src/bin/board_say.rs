//! Read a transcript file and one request; print the chat line and the board elements a chosen adapter writes.

use clap::{Parser, ValueEnum};
use parley_artifacts::board::BoardDoc;
use parley_brain::{DEFAULT_NAME, Transcript, find_request};
use parley_llm::{LlmProvider, anthropic::Anthropic, openai::OpenAiCompatible};
use std::path::PathBuf;

#[derive(Clone, ValueEnum)]
enum Adapter {
    Openai,
    Anthropic,
}

#[derive(Parser)]
#[command(about = "Draw a board for a \"Parley, ...\" request (config from env, see README)")]
struct Args {
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// The bot name that starts a request.
    #[arg(long, default_value = DEFAULT_NAME)]
    name: String,
    /// Transcript file, one `Speaker: text` per line (may be empty).
    transcript: PathBuf,
    /// The request, for example "Parley, draw the signup flow".
    request: String,
}

async fn run<P: LlmProvider>(
    p: P,
    t: &Transcript,
    q: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let board = BoardDoc::new();
    println!("{}", t.write_board(&p, q, &board).await?);
    println!("{}", serde_json::to_string_pretty(&board.elements()?)?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let Some(q) = find_request(&args.request, &args.name) else {
        println!("not a request");
        return Ok(());
    };
    let mut t = Transcript::new();
    for (i, l) in std::fs::read_to_string(&args.transcript)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
    {
        let (speaker, text) = l.split_once(':').ok_or("expected `Speaker: text`")?;
        t.push(parley_brain::Line {
            speaker: speaker.trim().into(),
            start_ms: i as u64 * 1000,
            text: text.trim().into(),
        });
    }
    match args.adapter {
        Adapter::Openai => run(OpenAiCompatible::from_env()?, &t, q).await,
        Adapter::Anthropic => run(Anthropic::from_env()?, &t, q).await,
    }
}
