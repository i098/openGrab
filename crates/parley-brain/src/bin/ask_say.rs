//! Read a transcript file and one request; print the answer a chosen adapter gives.

use clap::{Parser, ValueEnum};
use parley_brain::{DEFAULT_NAME, Line, Transcript, find_request};
use parley_llm::{LlmProvider, anthropic::Anthropic, openai::OpenAiCompatible};
use std::path::PathBuf;

#[derive(Clone, ValueEnum)]
enum Adapter {
    Openai,
    Anthropic,
}

#[derive(Parser)]
#[command(
    about = "Answer a \"Parley, ...\" request from a transcript file (config from env, see README)"
)]
struct Args {
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// The bot name that starts a request.
    #[arg(long, default_value = DEFAULT_NAME)]
    name: String,
    /// One line per utterance: `Speaker: text`.
    transcript: PathBuf,
    /// The request, for example "Parley, what did Tom agree to?".
    request: String,
}

fn parse(src: &str) -> Result<Transcript, String> {
    let mut t = Transcript::new();
    for (i, raw) in src
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let (speaker, text) = raw
            .split_once(':')
            .ok_or_else(|| format!("line {}: expected `Speaker: text`", i + 1))?;
        // The file has no timings: line number stands in for the start time.
        t.push(Line {
            speaker: speaker.trim().into(),
            start_ms: i as u64 * 1000,
            text: text.trim().into(),
        });
    }
    Ok(t)
}

async fn run<P: LlmProvider>(
    p: P,
    t: &Transcript,
    q: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", t.answer(&p, q).await?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let Some(q) = find_request(&args.request, &args.name) else {
        println!("not a request");
        return Ok(());
    };
    let t = parse(&std::fs::read_to_string(&args.transcript)?)?;
    match args.adapter {
        Adapter::Openai => run(OpenAiCompatible::from_env()?, &t, q).await,
        Adapter::Anthropic => run(Anthropic::from_env()?, &t, q).await,
    }
}
